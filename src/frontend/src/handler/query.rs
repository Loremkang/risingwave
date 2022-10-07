// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use futures::StreamExt;
use pgwire::pg_field_descriptor::PgFieldDescriptor;
use pgwire::pg_response::{PgResponse, PgResultSet, StatementType};
use risingwave_common::error::{ErrorCode, Result, RwError};
use risingwave_common::session_config::QueryMode;
use risingwave_sqlparser::ast::Statement;
use tracing::debug;

use crate::binder::{Binder, BoundStatement};
use crate::handler::privilege::{check_privileges, resolve_privileges};
use crate::handler::util::{force_local_mode, to_pg_field};
use crate::planner::Planner;
use crate::scheduler::{
    BatchPlanFragmenter, ExecutionContext, ExecutionContextRef, LocalQueryExecution,
};
use crate::session::{OptimizerContext, SessionImpl};

pub type QueryResultSet = PgResultSet;

pub async fn handle_query(
    context: OptimizerContext,
    stmt: Statement,
    format: bool,
) -> Result<PgResponse> {
    let stmt_type = to_statement_type(&stmt);
    let session = context.session_ctx.clone();

    let bound = {
        let mut binder = Binder::new(&session);
        binder.bind(stmt)? // Assuming the AST is correct, maybe the binder does not bind to the
                           // correct columns?
                           // I believe the bound statement looks correct
    };

    let check_items = resolve_privileges(&bound);
    check_privileges(&session, &check_items)?;

    let query_mode = if force_local_mode(&bound) {
        debug!("force query mode to local");
        QueryMode::Local
    } else {
        session.config().get_query_mode()
    };
    debug!("query_mode:{:?}", query_mode);

    let (mut row_stream, pg_descs) = match query_mode {
        QueryMode::Local => {
            if stmt_type.is_dml() {
                // insert statements take this branch
                // Assume that things break here.
                // DML do not support local mode yet.
                distribute_execute(context, bound, format).await?
            } else {
                local_execute(context, bound, format).await?
            }
        }
        // Local mode do not support cancel tasks.
        QueryMode::Distributed => distribute_execute(context, bound, format).await?,
    };

    let rows_count = match stmt_type {
        StatementType::SELECT => None,
        StatementType::INSERT | StatementType::DELETE | StatementType::UPDATE => {
            // Get the row from the row_stream.
            let first_row_set = row_stream
                .next()
                .await
                .expect("compute node should return affected rows in output")
                .map_err(|err| RwError::from(ErrorCode::InternalError(format!("{}", err))))?;
            let affected_rows_str = first_row_set[0].values()[0]
                .as_ref()
                .expect("compute node should return affected rows in output");
            Some(
                String::from_utf8(affected_rows_str.to_vec())
                    .unwrap()
                    .parse()
                    .unwrap_or_default(),
            )
        }
        _ => unreachable!(),
    };

    // Implicitly flush the writes.
    if session.config().get_implicit_flush() {
        flush_for_write(&session, stmt_type).await?;
    }

    Ok(PgResponse::new_for_stream(
        stmt_type, rows_count, row_stream, pg_descs,
    ))
}

fn to_statement_type(stmt: &Statement) -> StatementType {
    use StatementType::*;

    match stmt {
        Statement::Query(_) => SELECT,
        Statement::Insert { .. } => INSERT,
        Statement::Delete { .. } => DELETE,
        Statement::Update { .. } => UPDATE,
        _ => unreachable!(),
    }
}

// Don't really understand this. Does this really execute the plan or only create and split the
// plan?
pub async fn distribute_execute(
    context: OptimizerContext,
    stmt: BoundStatement,
    format: bool,
) -> Result<(QueryResultSet, Vec<PgFieldDescriptor>)> {
    let session = context.session_ctx.clone();
    // Subblock to make sure PlanRef (an Rc) is dropped before `await` below.
    let (query, pg_descs) = {
        // Logical insert seems to ignore columns, because the backend is unable to process them?
        // see Planner::plan_insert
        let root = Planner::new(context.into()).plan(stmt)?;

        let pg_descs = root
            .schema()
            .fields()
            .iter()
            .map(to_pg_field)
            .collect::<Vec<PgFieldDescriptor>>();

        // pg_descs
        // [PgFieldDescriptor { name: "", table_oid: 0, col_attr_num: 0, type_oid: BigInt, type_len:
        // 8, type_modifier: -1, format_code: 0 }]

        let plan = root.gen_batch_distributed_plan()?; // have a look at this

        tracing::trace!(
            "Generated distributed plan: {:?}",
            plan.explain_to_string()?
        );
        // Where where are v1 and v2 here? Do we expect that the columns are
        // mentioned here? insert into t (v1, v2) values (1, 2);
        // PSQL explain: Insert on t (cost=0.00..0.01 rows=1 width=8)
        // Generated distributed plan:
        // BatchExchange { order: [], dist: Single }
        // └─BatchInsert { table: t }
        //  └─BatchValues { rows: [[1:Int32, 2:Int32]] }"

        let plan_fragmenter = BatchPlanFragmenter::new(
            session.env().worker_node_manager_ref(),
            session.env().catalog_reader().clone(),
        );
        let query = plan_fragmenter.split(plan)?;
        tracing::trace!("Generated query after plan fragmenter: {:?}", &query);
        // Does not really help me...
        // Generated query after plan fragmenter: Query { query_id: QueryId { id:
        // "1e519ed0-30f9-4ea1-9e43-9067ad2e5c96" }, stage_graph: StageGraph { root_stage_id: 0,
        // stages: {1: QueryStage { id: 1, parallelism: 1, exchange_info: ExchangeInfo { mode:
        // Single, distribution: None }, has_table_scan: false }, 0: QueryStage { id: 0,
        // parallelism: 1, exchange_info: ExchangeInfo { mode: Single, distribution: None },
        // has_table_scan: false }}, child_edges: {1: {}, 0: {1}}, parent_edges: {0: {}, 1: {0}} } }
        (query, pg_descs)
    };

    let execution_context: ExecutionContextRef = ExecutionContext::new(session.clone()).into();
    let query_manager = execution_context.session().env().query_manager().clone();
    Ok((
        query_manager
            .schedule(execution_context, query, format)
            .await?,
        pg_descs,
    ))
}

async fn local_execute(
    context: OptimizerContext,
    stmt: BoundStatement,
    format: bool,
) -> Result<(QueryResultSet, Vec<PgFieldDescriptor>)> {
    let session = context.session_ctx.clone();

    let timer = session
        .env()
        .frontend_metrics
        .latency_local_execution
        .start_timer();

    // Subblock to make sure PlanRef (an Rc) is dropped before `await` below.
    let (query, pg_descs) = {
        let root = Planner::new(context.into()).plan(stmt)?;

        let pg_descs = root
            .schema()
            .fields()
            .iter()
            .map(to_pg_field)
            .collect::<Vec<PgFieldDescriptor>>();

        let plan = root.gen_batch_local_plan()?;

        let plan_fragmenter = BatchPlanFragmenter::new(
            session.env().worker_node_manager_ref(),
            session.env().catalog_reader().clone(),
        );
        let query = plan_fragmenter.split(plan)?;
        tracing::trace!("Generated query after plan fragmenter: {:?}", &query);
        (query, pg_descs)
    };

    let front_env = session.env();

    let rsp = {
        // Acquire hummock snapshot for local execution.
        let hummock_snapshot_manager = front_env.hummock_snapshot_manager();
        let query_id = query.query_id().clone();
        let epoch = hummock_snapshot_manager
            .acquire(&query_id)
            .await?
            .committed_epoch;

        // TODO: Passing sql here
        let execution =
            LocalQueryExecution::new(query, front_env.clone(), "", epoch, session.auth_context());
        let rsp = Ok((execution.stream_rows(format), pg_descs));

        // Release hummock snapshot for local execution.
        hummock_snapshot_manager.release(epoch, &query_id).await;

        rsp
    };

    // Collect metrics
    timer.observe_duration();
    session
        .env()
        .frontend_metrics
        .query_counter_local_execution
        .inc();

    rsp
}

async fn flush_for_write(session: &SessionImpl, stmt_type: StatementType) -> Result<()> {
    match stmt_type {
        StatementType::INSERT | StatementType::DELETE | StatementType::UPDATE => {
            let client = session.env().meta_client();
            let snapshot = client.flush(true).await?;
            session
                .env()
                .hummock_snapshot_manager()
                .update_epoch(snapshot);
        }
        _ => {}
    }
    Ok(())
}

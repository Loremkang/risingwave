#!/usr/bin/env bash
set -e

echo "$(date) $*"

duration=600
compaction_config=""
create_compaction_group=0
keep_cluster=0
while getopts 'd:c:gk' opt; do
  case ${opt} in
    d )
      duration=${OPTARG}
      ;;
    c )
      compaction_config=${OPTARG}
      ;;
    g )
      create_compaction_group=1
      ;;
    k )
      keep_cluster=1
      ;;
    * )
      exit 1
      ;;
  esac
done

./risedev compose-deploy compose-3node-deploy
./risedev apply-compose-deploy

export RW_HUMMOCK_URL="hummock+s3://rw-bench-zhengwang-risingwave-hummock"
export RW_META_ADDR="http://127.0.0.1:5690"

psql -d dev -h 127.0.0.1 -p 4566 -U root -f input/create_source.sql
if [ ${create_compaction_group} -eq 1 ]; then
  psql -d dev -h 127.0.0.1 -p 4566 -U root -f input/create_mv_create_group.sql
else
  psql -d dev -h 127.0.0.1 -p 4566 -U root -f input/create_mv.sql
fi
./risedev ctl meta pause

compaction_group_ids=$(./risedev ctl hummock list-compaction-group|awk '/CompactionGroup/{x=NR+1}(NR==x){print $2}'|awk -F',' '{print $1}')
update_compaction_config="./risedev ctl hummock update-compaction-config"
echo "${compaction_group_ids}"
for compaction_group_id in ${compaction_group_ids}
do
    update_compaction_config+=" --compaction-group-ids=${compaction_group_id} "
done
update_compaction_config+="${compaction_config}"
eval "${update_compaction_config}"
./risedev ctl meta resume

echo "Will run ${duration} seconds"
sleep "${duration}"
#./risedev ctl meta pause
if [ ${keep_cluster} -eq 0 ]; then
  ./risedev apply-compose-deploy -2
fi

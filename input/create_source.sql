create source person
    (id INTEGER, name VARCHAR, "email_address" VARCHAR, "credit_card" VARCHAR, city VARCHAR, state VARCHAR, "date_time" TIMESTAMP)
with (
  connector = 'nexmark',
  nexmark.table.type = 'Person',
  nexmark.split.num = '12',
  nexmark.min.event.gap.in.ns = '0'
) row format json;

create source auction (id INTEGER, "item_name" VARCHAR, description VARCHAR, "initial_bid" INTEGER, reserve INTEGER, "date_time" TIMESTAMP, expires TIMESTAMP, seller INTEGER, category INTEGER) 
with (
  connector = 'nexmark',
  nexmark.table.type = 'Auction',
  nexmark.split.num = '12',
  nexmark.min.event.gap.in.ns = '0'
) row format json;

create source bid (auction INTEGER, bidder INTEGER, price INTEGER, "date_time" TIMESTAMP)
with (
  connector = 'nexmark',
  nexmark.table.type = 'Bid',
  nexmark.split.num = '12',
  nexmark.min.event.gap.in.ns = '0'
) row format json;

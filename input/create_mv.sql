CREATE MATERIALIZED VIEW nexmark_q7_1 AS
SELECT
    B.auction,
    B.price,
    B.bidder,
    B.date_time
FROM
    bid B
        JOIN (
        SELECT
            MAX(price) AS maxprice,
            window_end as date_time
        FROM
            TUMBLE(bid, date_time, INTERVAL '10' SECOND)
        GROUP BY
            window_end
    ) B1 ON B.price = B1.maxprice
WHERE
    B.date_time BETWEEN B1.date_time - INTERVAL '10' SECOND
        AND B1.date_time;

CREATE MATERIALIZED VIEW nexmark_q7_2 AS
SELECT
    B.auction,
    B.price,
    B.bidder,
    B.date_time
FROM
    bid B
        JOIN (
        SELECT
            MAX(price) AS maxprice,
            window_end as date_time
        FROM
            TUMBLE(bid, date_time, INTERVAL '10' SECOND)
        GROUP BY
            window_end
    ) B1 ON B.price = B1.maxprice
WHERE
    B.date_time BETWEEN B1.date_time - INTERVAL '10' SECOND
        AND B1.date_time;

CREATE MATERIALIZED VIEW nexmark_q7_3 AS
SELECT
    B.auction,
    B.price,
    B.bidder,
    B.date_time
FROM
    bid B
        JOIN (
        SELECT
            MAX(price) AS maxprice,
            window_end as date_time
        FROM
            TUMBLE(bid, date_time, INTERVAL '10' SECOND)
        GROUP BY
            window_end
    ) B1 ON B.price = B1.maxprice
WHERE
    B.date_time BETWEEN B1.date_time - INTERVAL '10' SECOND
        AND B1.date_time;

CREATE MATERIALIZED VIEW nexmark_q7_4 AS
SELECT
    B.auction,
    B.price,
    B.bidder,
    B.date_time
FROM
    bid B
        JOIN (
        SELECT
            MAX(price) AS maxprice,
            window_end as date_time
        FROM
            TUMBLE(bid, date_time, INTERVAL '10' SECOND)
        GROUP BY
            window_end
    ) B1 ON B.price = B1.maxprice
WHERE
    B.date_time BETWEEN B1.date_time - INTERVAL '10' SECOND
        AND B1.date_time;

CREATE MATERIALIZED VIEW nexmark_q7_5 AS
SELECT
    B.auction,
    B.price,
    B.bidder,
    B.date_time
FROM
    bid B
        JOIN (
        SELECT
            MAX(price) AS maxprice,
            window_end as date_time
        FROM
            TUMBLE(bid, date_time, INTERVAL '10' SECOND)
        GROUP BY
            window_end
    ) B1 ON B.price = B1.maxprice
WHERE
    B.date_time BETWEEN B1.date_time - INTERVAL '10' SECOND
        AND B1.date_time;
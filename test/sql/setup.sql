CREATE EXTENSION zombodb;
CREATE SCHEMA postgis; CREATE EXTENSION postgis SCHEMA postgis;
ALTER DATABASE contrib_regression SET search_path = public, dsl;
ALTER DATABASE contrib_regression SET max_parallel_workers_per_gather TO 0;
SELECT zdb.enable_postgis_support();
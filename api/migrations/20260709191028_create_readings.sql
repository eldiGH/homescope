CREATE EXTENSION IF NOT EXISTS timescaledb;

CREATE TABLE readings (
	time		TIMESTAMPTZ		NOT NULL,
	device_id	BIGINT			NOT NULL,
	seq		BIGINT			NOT NULL,
	temp_degc	DOUBLE PRECISION	NOT NULL,
	rh_percent	DOUBLE PRECISION	NOT NULL,
	battery_mv	INTEGER			NOT NULL,
	rssi		SMALLINT		NOT NULL
);

SELECT create_hypertable('readings', by_range('time'));

CREATE INDEX ON readings (device_id, time DESC);

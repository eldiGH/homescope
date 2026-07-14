-- Add devices table
CREATE TABLE devices (
	id		INTEGER		GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	hardware_id	BIGINT		NOT NULL UNIQUE,
	name		TEXT		NOT NULL
);

INSERT INTO devices (hardware_id, name)
	SELECT DISTINCT device_id, 'Node-' || LPAD(UPPER(TO_HEX(device_id)), 16, '0') FROM readings;

-- add readings_new table
CREATE TABLE readings_new (
	time		TIMESTAMPTZ		NOT NULL,
	device_id	INT			NOT NULL REFERENCES devices(id),
	seq		BIGINT			NOT NULL,
	temp_degc	DOUBLE PRECISION	NOT NULL,
	rh_percent	DOUBLE PRECISION	NOT NULL,
	battery_mv	INTEGER			NOT NULL,
	rssi		SMALLINT		NOT NULL
);

SELECT create_hypertable('readings_new', by_range('time'));


-- copy correctly related data to new table
INSERT INTO readings_new (time, device_id, seq, temp_degc, rh_percent, battery_mv, rssi)
SELECT r.time, d.id, r.seq, r.temp_degc, r.rh_percent, r.battery_mv, r.rssi 
	FROM readings r
	INNER JOIN devices d ON d.hardware_id = device_id;


-- remove old and rename new readings tables
DROP TABLE readings;
ALTER TABLE readings_new RENAME TO readings;

-- add index to new readings table
CREATE INDEX readings_device_id_time_idx ON readings (device_id, time DESC);

-- rename auto created index to remove _new from name
ALTER INDEX readings_new_time_idx RENAME TO readings_time_idx;

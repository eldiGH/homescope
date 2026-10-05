-- device_addr becomes the devices primary key and readings reference it
-- directly. It is unique, immutable (FICR-derived), always known at
-- provisioning, and already what the device key is bound to (AAD).
--
-- Every object below is dropped and created by its real name, so a schema
-- that is not what this migration expects fails here instead of silently.

-- readings: backfill the new reference
ALTER TABLE readings ADD COLUMN device_addr BIGINT;

UPDATE readings r
	SET device_addr = d.device_addr
	FROM devices d
	WHERE d.id = r.device_id;

ALTER TABLE readings ALTER COLUMN device_addr SET NOT NULL;

-- readings: drop everything built on device_id (names predate the
-- readings_new rename in 20260714133002)
ALTER TABLE readings DROP CONSTRAINT readings_new_device_id_fkey;
ALTER TABLE readings DROP CONSTRAINT readings_device_id_seq_time_key;
DROP INDEX readings_device_id_time_idx;
ALTER TABLE readings DROP COLUMN device_id;

-- devices: device_addr takes over the primary key
ALTER TABLE devices DROP CONSTRAINT devices_pkey;
ALTER TABLE devices DROP CONSTRAINT devices_device_addr_key;
ALTER TABLE devices ADD CONSTRAINT devices_pkey PRIMARY KEY (device_addr);
ALTER TABLE devices DROP COLUMN id;

-- readings: the same three objects, rebuilt on device_addr
ALTER TABLE readings ADD CONSTRAINT readings_device_addr_fkey
	FOREIGN KEY (device_addr) REFERENCES devices (device_addr);
ALTER TABLE readings ADD CONSTRAINT readings_device_addr_seq_time_key
	UNIQUE (device_addr, seq, time);
CREATE INDEX readings_device_addr_time_idx ON readings (device_addr, time DESC);

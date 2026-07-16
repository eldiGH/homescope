ALTER TABLE devices RENAME COLUMN hardware_id TO device_addr;
ALTER TABLE devices ADD CONSTRAINT device_addr_is_48_bits
	CHECK (device_addr BETWEEN 0 and 281474976710655);
ALTER INDEX devices_hardware_id_key RENAME TO devices_device_addr_key;

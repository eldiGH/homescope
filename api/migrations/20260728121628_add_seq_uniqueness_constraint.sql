ALTER TABLE readings ADD CONSTRAINT readings_device_id_seq_time_key UNIQUE (device_id, seq, time);

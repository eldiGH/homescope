-- Where each device is, as owner-asserted metadata with its own timeline. See
-- docs/design/site-room-topology.md § Revised 2026-10-05. No rows are seeded:
-- site and room names belong to whoever deploys this.

-- One identifier per site, shared by the gateway's SITE, the MQTT topic segment
-- and the broker user homescope-<name>, so it obeys the topic-level grammar.
CREATE TABLE sites (
	id		INTEGER			GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	name		TEXT			NOT NULL,
	display_name	TEXT,
	-- for reducing station pressure to sea level
	elevation_m	DOUBLE PRECISION,

	CONSTRAINT sites_name_key UNIQUE (name),
	CONSTRAINT site_name_is_one_topic_level CHECK (name ~ '^[a-z0-9-]+$')
);

CREATE TABLE rooms (
	id		INTEGER			GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	site_id		INTEGER			NOT NULL,
	name		TEXT			NOT NULL,

	CONSTRAINT rooms_site_id_fkey FOREIGN KEY (site_id)
		REFERENCES sites (id) ON DELETE RESTRICT,
	CONSTRAINT rooms_site_id_name_key UNIQUE (site_id, name)
);

-- A placement lasts until the device's next placed_at. room_id NULL means
-- taken down. Move = insert a row; correction = update one.
CREATE TABLE device_placements (
	device_addr	BIGINT			NOT NULL,
	room_id		INTEGER,
	placed_at	TIMESTAMPTZ		NOT NULL DEFAULT now(),

	CONSTRAINT device_placements_pkey PRIMARY KEY (device_addr, placed_at),
	CONSTRAINT device_placements_device_addr_fkey FOREIGN KEY (device_addr)
		REFERENCES devices (device_addr),
	CONSTRAINT device_placements_room_id_fkey FOREIGN KEY (room_id)
		REFERENCES rooms (id) ON DELETE RESTRICT
);

-- Each placement with the moment it ended. valid_to NULL = still there, so a
-- reading belongs to a placement when
--   time >= valid_from AND (valid_to IS NULL OR time < valid_to)
CREATE VIEW placement_periods AS
SELECT
	device_addr,
	room_id,
	placed_at AS valid_from,
	lead(placed_at) OVER (PARTITION BY device_addr ORDER BY placed_at) AS valid_to
FROM device_placements;

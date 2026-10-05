-- Dev-only seed: ~90 days of per-minute readings for six fake devices.
-- Readings are generated only for the devices inserted here, so it's safe
-- to run on a dev DB that already holds real devices/readings.
--
--   podman exec -i homescope-homescope-db-1 psql -U postgres -d homescope < deploy/timescaledb/seed.dev.sql

-- n numbers the fake devices 1..6; it shapes each one's baseline, battery and
-- RSSI below, and stays the same whatever the DB already holds.
WITH fake AS (
	SELECT name, n
	FROM unnest(ARRAY['Living room', 'Bedroom', 'Kitchen', 'Office', 'Garage', 'Attic'])
		WITH ORDINALITY AS u(name, n)
),
new_devices AS (
	INSERT INTO devices (device_addr, name)
	SELECT random(0, 281474976710655), name FROM fake
	RETURNING device_addr, name
)
INSERT INTO readings (time, device_addr, seq, temp_degc, rh_percent, battery_mv, rssi)
SELECT
	ts,
	d.device_addr,
	-- seq = elapsed minutes since window start, NOT row_number(): window
	-- functions run after WHERE, so row_number() would renumber around the
	-- dropped rows and hide the very seq gaps the drop is meant to create
	(e.elapsed_days * 1440)::bigint,
	t.temp + (random() - 0.5) * 0.3,                                       -- sensor noise
	least(greatest(
		50 - 1.6 * (t.temp - 21)                                           -- RH loosely inverse to temp
			+ (random() - 0.5) * 3,
		25), 70),
	(3230 - f.n * 15 - 3.4 * e.elapsed_days + (random() - 0.5) * 8)::int, -- ~300 mV drain over the window
	(-60 - f.n * 5 + (random() - 0.5) * 20)::smallint                     -- per-device base + indoor fading
FROM generate_series(now() - interval '90 days', now(), interval '60 seconds') AS ts
CROSS JOIN new_devices AS d
JOIN fake AS f USING (name)
CROSS JOIN LATERAL (
	SELECT extract(epoch FROM ts - (now() - interval '90 days')) / 86400.0 AS elapsed_days
) AS e
CROSS JOIN LATERAL (
	SELECT
		-- room baseline: living spaces ~20.4-21.6 °C, garage cool, attic warm
		CASE d.name
			WHEN 'Garage' THEN 16.5
			WHEN 'Attic'  THEN 22.5
			ELSE 20.0 + f.n * 0.4
		END
		-- daily cycle peaking ~15:00 CEST; garage/attic swing harder
		+ CASE d.name
			WHEN 'Garage' THEN 2.5
			WHEN 'Attic'  THEN 5.0
			ELSE 1.3
		END * sin((extract(epoch FROM ts) / 86400.0 - 0.29) * 2 * pi())
		-- spring -> summer warm-up across the window
		+ 3.5 * e.elapsed_days / 90.0
		AS temp
) AS t
WHERE random() > 0.03;   -- ~3 % missing packets, so gap/delivery panels have something to show

CREATE TABLE `signal_rollups_hourly` (
	`environment_id` text NOT NULL,
	`signal_id` text NOT NULL,
	`hour_start` integer NOT NULL,
	`avg_real` real,
	`max_real` real,
	`sample_count` integer NOT NULL,
	PRIMARY KEY(`environment_id`, `signal_id`, `hour_start`)
);
--> statement-breakpoint
CREATE INDEX `signal_rollups_by_env_signal_hour` ON `signal_rollups_hourly` (`environment_id`,`signal_id`,`hour_start`);
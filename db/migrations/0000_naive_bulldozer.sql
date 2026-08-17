CREATE TABLE `signal_samples` (
	`environment_id` text NOT NULL,
	`signal_id` text NOT NULL,
	`observed_at` integer NOT NULL,
	`value_real` real,
	`value_json` text
);
--> statement-breakpoint
CREATE INDEX `signal_samples_by_env_signal_time` ON `signal_samples` (`environment_id`,`signal_id`,`observed_at`);--> statement-breakpoint
CREATE TABLE `signal_snapshots` (
	`environment_id` text NOT NULL,
	`signal_id` text NOT NULL,
	`observed_at` integer NOT NULL,
	`state` text NOT NULL,
	`payload_json` text NOT NULL,
	PRIMARY KEY(`environment_id`, `signal_id`)
);

CREATE TABLE `signal_events` (
	`environment_id` text NOT NULL,
	`signal_id` text NOT NULL,
	`observed_at` integer NOT NULL,
	`from_state` text,
	`to_state` text NOT NULL,
	PRIMARY KEY(`environment_id`, `signal_id`, `observed_at`)
);
--> statement-breakpoint
CREATE INDEX `signal_events_by_env_signal_time` ON `signal_events` (`environment_id`,`signal_id`,`observed_at`);--> statement-breakpoint
CREATE TABLE `signal_publish_state` (
	`environment_id` text NOT NULL,
	`signal_id` text NOT NULL,
	`last_state` text NOT NULL,
	`consecutive` integer NOT NULL,
	`previous_state` text,
	PRIMARY KEY(`environment_id`, `signal_id`)
);
--> statement-breakpoint
ALTER TABLE `health_events` ADD `note` text;
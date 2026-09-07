CREATE TABLE `dashboard_publish_state` (
	`environment_id` text PRIMARY KEY NOT NULL,
	`last_health` text NOT NULL,
	`consecutive` integer NOT NULL,
	`previous_health` text,
	`last_build` text
);
--> statement-breakpoint
CREATE TABLE `health_events` (
	`environment_id` text NOT NULL,
	`observed_at` integer NOT NULL,
	`kind` text NOT NULL,
	`from_health` text,
	`to_health` text NOT NULL,
	`build` text,
	PRIMARY KEY(`environment_id`, `observed_at`, `kind`)
);
--> statement-breakpoint
CREATE INDEX `health_events_by_env_time` ON `health_events` (`environment_id`,`observed_at`);
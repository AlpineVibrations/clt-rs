on the agent tui panel the doing column needs a tiny padding between it and the last run. its hard to read

Completed: Added an extra spacer between the DOING and LAST RUN columns in the agent project table, with regression coverage for compact and wide table formatting. Ran: cargo fmt; cargo test agent_project_table_pads_doing_before_last_run; cargo test.

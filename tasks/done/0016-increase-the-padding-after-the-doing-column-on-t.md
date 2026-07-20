increase the padding after the doing column on the agent panel in the tui

Completed: Increased the agent panel table gap between DOING and LAST RUN by one space, centralized the gap as a layout constant, and updated the focused formatting assertion. Ran: cargo fmt; cargo test agent_project_table_pads_doing_before_last_run; cargo test.

#!/bin/zsh

echo "--- Step 1: Adding a new task ---"
cargo run -- add "Test task for flow script"
echo ""

echo "--- Step 2: Listing todo tasks ---"
cargo run -- list todo
echo ""

echo "--- Step 3: Moving task from todo to doing ---"
cargo run -- status todo doing 1
echo ""

echo "--- Step 4: Listing doing tasks ---"
cargo run -- list doing
echo ""

echo "--- Step 5: Marking task as done ---"
cargo run -- done doing 1
echo ""

echo "--- Step 6: Listing done tasks ---"
cargo run -- list done
echo ""

echo "Flow test complete!"
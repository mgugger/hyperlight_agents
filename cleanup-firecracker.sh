#!/bin/bash
# Cleanup script for Firecracker processes

echo "Cleaning up Firecracker processes..."

# Find all firecracker processes
PIDS=$(pgrep -f firecracker)

if [ -z "$PIDS" ]; then
    echo "No Firecracker processes found"
    exit 0
fi

echo "Found Firecracker processes: $PIDS"

# First try graceful shutdown
for pid in $PIDS; do
    echo "Sending SIGTERM to PID $pid"
    kill -TERM $pid 2>/dev/null || echo "Failed to send SIGTERM to $pid"
done

# Wait a moment
sleep 2

# Check if any are still running and force kill
REMAINING=$(pgrep -f firecracker)
if [ ! -z "$REMAINING" ]; then
    echo "Force killing remaining processes: $REMAINING"
    for pid in $REMAINING; do
        echo "Sending SIGKILL to PID $pid"
        kill -KILL $pid 2>/dev/null || echo "Failed to send SIGKILL to $pid"
    done
fi

echo "Cleanup complete"

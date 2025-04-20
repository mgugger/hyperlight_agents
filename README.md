# Hyperlight  Guests / Agents

The demo implements a hyperlight agent that:
1. Fetches the top stories from Hacker News via HTTP requests
2. Processes the responses asynchronously using callbacks
3. Demonstrates secure host function calls from sandboxed environments

Each agent runs in its own isolated sandbox with controlled access to system resources. The architecture supports running multiple agents in parallel, each with its own communication channel and state.

## Run

```
cd guest && cargo build
cd host && cargo run
```

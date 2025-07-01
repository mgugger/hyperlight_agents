# Test Large Output Handling

This document provides test cases to validate the large output handling implementation.

## Test Plan

### Test 1: Basic Large Output (dmesg)
**Command:** `dmesg`
**Expected:** Should complete without timeout/channel errors
**Validation:**
- Look for: `DEBUG: Large result received successfully through channel system`
- No errors: `sending on a closed channel`
- No timeouts after 30 seconds

### Test 2: Very Large Output (find command)
**Command:** `find /usr -type f`
**Expected:** Should handle large directory listing
**Validation:**
- Progress messages: `DEBUG: Read X MB so far...`
- Memory monitoring: `DEBUG: Memory usage at...`
- Successful completion

### Test 3: Moderate Output (process list)
**Command:** `ps aux --forest`
**Expected:** Should complete normally
**Validation:**
- Normal processing without warnings
- Quick completion

### Test 4: Empty/Small Output
**Command:** `echo "test"`
**Expected:** Should work normally without large output handling
**Validation:**
- No large output warnings
- Fast completion

## Manual Testing Steps

1. **Start the hyperlight agents system**
   ```bash
   cd hyperlight_agents
   cargo run --bin hyperlight-agents-host
   ```

2. **Connect via MCP and test each command**
   Use your MCP client to execute:
   ```json
   {
     "method": "tools/call",
     "params": {
       "name": "vm_builder",
       "arguments": {
         "action": "execute_vm_command",
         "vm_id": "test_vm",
         "command": "dmesg"
       }
     }
   }
   ```

3. **Monitor the logs for expected debug messages**

## Expected Debug Output

### For Large Outputs (dmesg):
```
DEBUG: Starting to read response from VM...
DEBUG: Read 1 MB so far...
DEBUG: Read 2 MB so far...
DEBUG: Memory usage at during large output read: 156MB
DEBUG: Finished reading response, total bytes: 2847362
WARNING: Large command result detected: 2847362 bytes total
DEBUG: Command result sent successfully via channel
DEBUG: Large result received successfully through channel system
DEBUG: Returning successful result for command cmd_123 (output length: 2847362 bytes)
```

### For Normal Commands:
```
DEBUG: Starting to read response from VM...
DEBUG: Finished reading response, total bytes: 42
DEBUG: Command result sent successfully via channel
DEBUG: Returning successful result for command cmd_124 (output length: 15 bytes)
```

## Success Criteria

✅ **No timeout errors** - Commands complete within 5 minutes
✅ **No channel errors** - No "sending on a closed channel" messages  
✅ **Memory monitoring** - Memory usage logged for large outputs
✅ **Progress reporting** - MB progress messages for large outputs
✅ **Successful transmission** - Large results sent through channels
✅ **Output integrity** - Complete output received (or properly truncated)

## Failure Investigation

If tests still fail, check:

1. **Where exactly it fails:**
   - During VM response reading?
   - During JSON parsing?
   - During channel transmission?
   - During MCP response?

2. **Memory issues:**
   - Check memory usage logs
   - Look for truncation warnings
   - Monitor system memory

3. **Timeout location:**
   - VM agent timeout?
   - Host processing timeout?
   - MCP server timeout?

4. **Channel issues:**
   - Channel capacity problems?
   - Serialization failures?
   - VM process crashes?

## Performance Benchmarks

Record these metrics during testing:

- **Small output (< 1KB):** Should complete in < 1 second
- **Medium output (1KB-1MB):** Should complete in < 10 seconds  
- **Large output (1MB-10MB):** Should complete in < 60 seconds
- **Very large output (> 10MB):** Should be truncated with warning

## Test Environment Notes

- Ensure sufficient system memory (> 1GB available)
- Monitor system resources during tests
- Test on both debug and release builds
- Verify VM agent is functioning properly
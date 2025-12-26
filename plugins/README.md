# TDSR Plugin System

Plugins allow you to extend TDSR with custom output analysis and speech generation.

## How Plugins Work

1. **Trigger**: User presses a configured keyboard shortcut (e.g., alt+d)
2. **Collection**: TDSR collects screen lines from bottom to top until it finds the prompt
3. **Execution**: TDSR runs your plugin script as a subprocess
4. **Input**: Plugin receives JSON via stdin with screen lines and last command
5. **Processing**: Plugin analyzes the lines
6. **Output**: Plugin returns JSON via stdout with lines to speak
7. **Speech**: TDSR speaks the returned lines to the user

## Plugin Protocol

### Input (via stdin):
```json
{
  "lines": ["line 3", "line 2", "line 1"],
  "last_command": "ls -la"
}
```

### Output (via stdout):
```json
{
  "speak": ["Found 10 files", "2 directories"]
}
```

## Creating a Plugin

### 1. Write Your Plugin Script

Create a Python script (or any executable) in `~/.tdsr/plugins/`:

```python
#!/usr/bin/env python3
import json
import sys

def main():
    # Read input
    input_data = json.loads(sys.stdin.readline())
    lines = input_data['lines']
    last_command = input_data.get('last_command')

    # Analyze lines
    result = []
    # Your logic here...

    # Return output
    output = {'speak': result}
    print(json.dumps(output))

if __name__ == '__main__':
    main()
```

### 2. Make it Executable

```bash
chmod +x ~/.tdsr/plugins/my_plugin.py
```

### 3. Configure in ~/.tdsr.cfg

```ini
[plugins]
my_plugin = d

[commands]
my_plugin = ls.*
```

- `[plugins]` maps plugin name to keyboard shortcut
- `[commands]` (optional) filters when plugin runs based on last command regex

### 4. Configure Prompt Pattern (Optional)

```ini
[speech]
prompt = \$|>|#
```

The prompt pattern tells TDSR when to stop collecting lines. Default is `.*` (any line).

## Plugin Examples

### File Counter
Counts files in `ls` output:
```python
def parse_output(lines):
    count = len([l for l in lines if l.strip()])
    return [f"{count} items"]
```

### Error Detector
Finds errors in output:
```python
def parse_output(lines):
    errors = [l for l in lines if 'error' in l.lower()]
    if errors:
        return [f"Found {len(errors)} errors", errors[0]]
    return []
```

### Git Status Parser
Reads git status output:
```python
def parse_output(lines, last_command):
    if last_command and 'git status' in last_command:
        modified = [l for l in lines if 'modified:' in l]
        return [f"{len(modified)} files modified"]
    return []
```

## Nested Plugins

For organization, use dotted names:

```ini
[plugins]
git.status = g
```

Create directory structure:
```
~/.tdsr/plugins/
  git/
    status.py
```

## Debugging

1. Test plugin standalone:
```bash
echo '{"lines":["test"]}' | python3 ~/.tdsr/plugins/my_plugin.py
```

2. Check TDSR logs (if --debug flag used)

3. Plugin errors are spoken: "Plugin error: ..."

## Language Support

While Python is recommended, **any executable works**:
- Shell scripts
- Ruby, Perl, Node.js
- Compiled binaries

Just ensure they:
1. Read JSON from stdin
2. Write JSON to stdout
3. Are executable

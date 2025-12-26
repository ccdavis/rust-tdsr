#!/usr/bin/env python3
"""
Example TDSR plugin

This plugin demonstrates the subprocess-based plugin system.
It receives screen lines as JSON input and returns speech output as JSON.

Protocol:
- Input (stdin): JSON object with "lines" array and optional "last_command"
- Output (stdout): JSON object with "speak" array

To use:
1. Add to ~/.tdsr.cfg:
   [plugins]
   example = d

2. Press alt+d to run the plugin
"""

import json
import sys

def parse_output(lines, last_command=None):
    """
    Analyze terminal output and return lines to speak.

    Args:
        lines: List of screen lines from bottom to top (up to prompt)
        last_command: The last command executed (if available)

    Returns:
        List of strings to speak
    """
    # Example: Count non-empty lines
    non_empty = [line for line in lines if line.strip()]

    result = []
    result.append(f"Found {len(non_empty)} non-empty lines")

    if last_command:
        result.append(f"Last command was: {last_command}")

    # Example: Look for specific patterns
    error_lines = [line for line in lines if 'error' in line.lower()]
    if error_lines:
        result.append(f"Found {len(error_lines)} lines containing 'error'")

    return result

def main():
    # Read JSON input from stdin
    try:
        input_data = json.loads(sys.stdin.readline())
        lines = input_data.get('lines', [])
        last_command = input_data.get('last_command')

        # Process lines
        speech_lines = parse_output(lines, last_command)

        # Write JSON output to stdout
        output = {'speak': speech_lines}
        print(json.dumps(output))
        sys.exit(0)

    except Exception as e:
        # On error, return error message
        output = {'speak': [f"Plugin error: {str(e)}"]}
        print(json.dumps(output))
        sys.exit(1)

if __name__ == '__main__':
    main()

# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Record what a console program writes to its terminal, one file per keystroke.

Runs the program in an 80x25 pseudo-terminal (TERM=xterm), sends a scripted
sequence of keys, and stores the raw bytes the program wrote after each key
as tests/fixtures/<name>/NN_<step>.bin, with an index in steps.txt. The
fixtures drive tests/tui_test.rs, so the TUI tracker is tested against the
real output of the Free Pascal IDE, whiptail and friends.

    uv run tools/capture_tui.py fp        # needs the fp binary
    uv run tools/capture_tui.py whiptail
    uv run tools/capture_tui.py ls less   # shell-like output, must not activate TUI mode

Bytes read from the pty are the program's output only; the keys sent are
listed in steps.txt for reference.
"""
import os
import pty
import select
import signal
import struct
import sys
import tempfile
import termios
import fcntl
import time

ROWS, COLS = 25, 80
ESC = b"\x1b"
UP, DOWN, RIGHT, LEFT = b"\x1b[A", b"\x1b[B", b"\x1b[C", b"\x1b[D"
# What a terminal sends for the arrows once a program has switched the
# keypad to application mode (mc does)
APP_UP, APP_DOWN, APP_RIGHT, APP_LEFT = b"\x1bOA", b"\x1bOB", b"\x1bOC", b"\x1bOD"
F10 = b"\x1b[21~"
# fp reads a lone ESC as the start of an Alt sequence and holds it until the
# next byte arrives; a second ESC sent separately a moment later is what
# registers as the Escape key (two ESCs in one write are not).
ESCAPE = [(ESC, 0.3), (ESC, 0.0)]

SCENARIOS = {
    "fp": {
        "argv": ["fp"],
        # Alt+letter keys are TDSR's own (Alt+o, Alt+x...), so menus are
        # reached with F10 and the arrows, as a TDSR user would.
        "steps": [
            ("start", None),
            ("f10_menu", F10),
            ("right_edit", RIGHT),
            ("down_open_edit_menu", DOWN),
            ("down_no_move", DOWN),
            ("escape_close_menu", ESCAPE),
            ("alt_f_file_menu", ESC + b"f"),
            ("down_new_from_template", DOWN),
            ("up_new", UP),
            ("enter_new_editor", b"\r"),
            ("type_a", b"a"),
            ("type_b", b"b"),
            ("type_c", b"c"),
            ("left", LEFT),
            ("left_again", LEFT),
            ("enter_split_line", b"\r"),
            ("up_line", UP),
            ("down_line", DOWN),
            ("f10_menu_again", F10),
            ("left_help", LEFT),
            ("left_window", LEFT),
            ("left_options", LEFT),
            ("down_open_options_menu", DOWN),
            ("enter_mode_dialog", b"\r"),
            ("tab_ok", b"\t"),
            ("tab_cancel", b"\t"),
            ("tab_back_to_radio", b"\t"),
            ("down_debug_radio", DOWN),
            ("escape_close_dialog", ESCAPE),
            ("alt_f_file_menu_again", ESC + b"f"),
            ("up_wrap_to_exit", UP),
            ("enter_exit_prompt", b"\r"),
            ("n_dont_save", b"n"),
        ],
    },
    "whiptail": {
        "argv": ["whiptail", "--title", "Pick one", "--menu", "Choose a thing to do",
                 "15", "60", "4", "open", "Open a file", "save", "Save the file",
                 "quit", "Leave now"],
        "steps": [
            ("start", None),
            ("down_save", DOWN),
            ("down_quit", DOWN),
            ("tab_ok", b"\t"),
            ("tab_cancel", b"\t"),
            ("enter", b"\r"),
        ],
    },
    "mc": {
        # -u: no subshell (it would wait for a prompt in this bare pty)
        "argv": ["mc", "-u"],
        "steps": [
            ("start", None),
            ("down", APP_DOWN),
            ("down_again", APP_DOWN),
            ("tab_other_panel", b"\t"),
            ("down_other_panel", APP_DOWN),
            ("tab_back", b"\t"),
            ("f9_menu", b"\x1b[20~"),
            ("right_menu", APP_RIGHT),
            ("down_open_menu", APP_DOWN),
            ("down_next_item", APP_DOWN),
            ("escape_close_menu", ESCAPE),
            ("end_last_file", b"\x1bOF"),
            ("f3_view", b"\x1bOR"),
            ("f3_close_view", b"\x1bOR"),
            ("f10_quit", b"\x1b[21~"),
        ],
        "files": {"notes.txt": "first line of notes\nsecond line\nthird line\n"},
    },
    "nano": {
        "argv": ["nano", "notes.txt"],
        "steps": [
            ("start", None),
            ("type_h", b"h"),
            ("type_i", b"i"),
            ("enter", b"\r"),
            ("type_x", b"x"),
            ("up", UP),
            ("down", DOWN),
            ("ctrl_g_help", b"\x07"),
            ("ctrl_x_close_help", b"\x18"),
            ("ctrl_x_exit", b"\x18"),
            ("n_dont_save", b"n"),
        ],
    },
    "ls": {
        "argv": ["ls", "--color=always", "-la", "/etc"],
        "steps": [("start", None)],
    },
    "less": {
        "argv": ["less", "/etc/services"],
        "steps": [("start", None), ("down", DOWN), ("page_down", b" "), ("quit", b"q")],
    },
}


def drain(fd, first_timeout, settle=0.15):
    buf = b""
    deadline = time.time() + first_timeout
    while True:
        remaining = deadline - time.time()
        if remaining <= 0:
            break
        ready, _, _ = select.select([fd], [], [], remaining)
        if not ready:
            break
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
        deadline = time.time() + settle
    return buf


def capture(name):
    scenario = SCENARIOS[name]
    out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tests", "fixtures", name)
    out_dir = os.path.normpath(out_dir)
    os.makedirs(out_dir, exist_ok=True)
    for old in os.listdir(out_dir):
        os.remove(os.path.join(out_dir, old))
    workdir = tempfile.mkdtemp(prefix="tdsr-capture-")
    for name, contents in scenario.get("files", {}).items():
        with open(os.path.join(workdir, name), "w") as f:
            f.write(contents)

    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        os.environ["TERM"] = "xterm"
        os.environ["HOME"] = workdir
        os.environ["LINES"] = str(ROWS)
        os.environ["COLUMNS"] = str(COLS)
        os.environ["LANG"] = "C.UTF-8"
        os.execvp(scenario["argv"][0], scenario["argv"])

    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    index = []
    for i, (step, keys) in enumerate(scenario["steps"]):
        if isinstance(keys, list):
            for chunk, pause in keys:
                os.write(fd, chunk)
                time.sleep(pause)
        elif keys is not None:
            os.write(fd, keys)
        raw = drain(fd, 1.5 if keys is None else 0.8)
        fname = f"{i:02d}_{step}.bin"
        with open(os.path.join(out_dir, fname), "wb") as f:
            f.write(raw)
        index.append(f"{fname}\t{keys!r}")
        print(f"{fname}: {len(raw)} bytes")
    with open(os.path.join(out_dir, "steps.txt"), "w") as f:
        f.write("# fixture file\tkeys sent before it (Python bytes literal)\n")
        f.write("\n".join(index) + "\n")
    try:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
    except OSError:
        pass


if __name__ == "__main__":
    names = sys.argv[1:] or ["fp", "whiptail"]
    for n in names:
        print(f"== {n}")
        capture(n)

#!/usr/bin/env python3
"""Runs a command on a pseudo-terminal and answers its prompts.

    pty-drive.py --answer 'Password for homescope-api=secret' [...] -- CMD ARGS...

Each --answer is PROMPT=VALUE: when PROMPT first appears in the output, VALUE
and a newline are typed. An empty VALUE answers with a bare newline. The
command's output is copied to stdout; the exit status is the command's.

Used by vm.sh to exercise deploy.sh's interactive path — `read -s` needs a
terminal, which ssh without -t does not provide.
"""

import os
import pty
import select
import sys


def main(argv):
    if "--" not in argv:
        print(__doc__, file=sys.stderr)
        return 2
    split = argv.index("--")
    options, command = argv[1:split], argv[split + 1 :]

    answers = {}
    while options:
        flag, spec, *options = options
        if flag != "--answer" or "=" not in spec:
            print(f"bad option: {flag} {spec}", file=sys.stderr)
            return 2
        prompt, value = spec.split("=", 1)
        answers[prompt.encode()] = value.encode() + b"\n"

    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(command[0], command)

    seen, answered = b"", set()
    while True:
        ready, _, _ = select.select([fd], [], [], 1800)
        if not ready:
            break
        try:
            data = os.read(fd, 4096)
        except OSError:  # EIO: the child closed the terminal
            break
        if not data:
            break
        seen += data
        sys.stdout.buffer.write(data)
        sys.stdout.flush()
        for prompt, value in answers.items():
            if prompt in seen and prompt not in answered:
                os.write(fd, value)
                answered.add(prompt)

    _, status = os.waitpid(pid, 0)
    return os.waitstatus_to_exitcode(status)


if __name__ == "__main__":
    sys.exit(main(sys.argv))

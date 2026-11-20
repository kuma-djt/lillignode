# Security

Lillignode starts programs, passes your typing to them, and reads files on your machine. A fault in it can matter. Reports are welcome.

## Reporting a problem

**Do not open a public issue for a security problem.**

Use GitHub's private reporting: on this repository's **Security** tab, choose **Report a vulnerability**. That goes only to the maintainers.

Please include what you did, what happened, what you expected, your operating system and terminal, and the Lillignode version (`lillignode --version`). Leave out anything secret: no API keys, no conversation contents.

## What to expect

Lillignode is maintained by a very small team and is at v0.2. There is no guaranteed response time. You should hear back within about a week. If a fix is needed, you will be told when it is out, and credited if you want to be.

## Supported versions

Only the latest release and the `main` branch. There are no back-ported fixes.

## What counts

- Typing or a paste reaching an agent, or being submitted to it, when the user did not intend it.
- Lillignode reading, writing or starting something outside what the README says it touches.
- Text from an agent's output or from a file Lillignode reads (a saved workspace, a settings file, a Claude Code conversation record) being able to draw over Lillignode's own screen, run a command, or name a file it should not.
- A key, token or conversation content ending up on disk or leaving the machine through Lillignode.
- The Windows packaging script accepting a file that is not the pinned, Microsoft-signed one.

## Known limits

Some weaknesses are known and not yet fixed in 0.2. They are listed in plain words under [Known limits in 0.2](README.md#known-limits-in-02) in the README: the saved workspace is trusted at launch within limits, a paste on Windows can still carry control characters, cost is experimental, and only Windows has been run by a person. A report that finds a new way to exploit one of them is still welcome.

## What does not

- What an agent itself does once you have given it a task. That is the agent's own security model.
- Problems in Claude Code or another agent. Report those to whoever makes them.
- Anything that needs an attacker who can already run programs as you.

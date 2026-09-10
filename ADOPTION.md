# Adoption

How Tessra becomes more popular than git. Companion to VISION.md, which says what to build. This says how it wins.

## Why git won

Git did not win because it was the best version control system. It won because of things that mostly happened outside the tool.

- **A demanding first user with credibility.** Linus Torvalds and the Linux kernel. It had to be fast and correct on the hardest project in the world, and everyone knew who built it.
- **One operation became dramatically cheaper.** Branching went from minutes to milliseconds. That speedup created feature branches, and feature branches became the culture.
- **Free forking.** Every clone is the whole repository. No permission needed to start.
- **GitHub.** Git was ugly and hard. GitHub made it social: forks as a gesture, pull requests as the unit of collaboration, stars as reputation. Mercurial was nicer and lost because GitHub picked git. The platform made the tool popular, not the reverse.
- **Free, open, stable.** No vendor. A data model unchanged since 2005, so everything built on it kept working and it ended up in every IDE, CI, and host.
- **Fear removed.** The reflog meant nothing was ever lost. People take risks with tools they trust.

Every "git but better" lost: Mercurial, Pijul, Fossil, Sapling outside Meta. Jujutsu is the exception, and it survives on one trick: it runs inside an existing git repository, so switching cost is zero.

## The lesson

Better is not enough. Popularity needs four things: a wedge, zero switching cost, a platform with network effects, and a workflow that becomes culture.

## The path

### Agents are the new developers

Every prior version control system had to convert humans with decades of muscle memory. An agent switches tools by reading two thousand tokens of documentation. If agent frameworks default to Tessra because their agents measurably do better on it, humans follow the results. The swarm is the demanding first user, and it is where all the growth is.

### Memory is the first wedge

Every developer using an agent today maintains a CLAUDE.md, an AGENTS.md, or a cursorrules file by hand, and it goes stale the moment the code changes. Tessra replaces the file with memory in the graph: agents record what they learn, every tool shares it, and a generated file covers tools that do not speak Tessra yet. It needs only the object store and one verb, so it ships in M1 and it is the first thing anyone installs Tessra for. The pitch is one sentence: your agents stop relearning the same things. See MEMORY.md.

### The review bottleneck is the second wedge

Every team with agents has a review queue growing faster than anyone can read it. Tessra lets people define standards once and proves every change against them before it lands, so humans review exceptions and samples instead of diffs. This is the pain teams feel today, and it is what makes fully automated flows trustworthy enough to turn on. See PIPELINE.md.

### No UI to build, no UI to learn

Tessra ships no UI. Humans get documents on demand through their agent and through channels they already use. That removes the largest cost in every developer tool and the largest barrier to adoption: there is nothing to learn, only questions to ask. Anyone can build a visual layer on the query language, and the hub may be one, but the substrate never waits for it. See LIFECYCLE.md.

### Zero switching cost

Tessra runs inside an existing git repository from day one. GitHub keeps working. The team never has to know. Colocation is part of M1, not a late bridge. The M0 object model spec is locked first so git cannot distort it. Existing CI and deploy tools keep running too: hooks trigger them and their results come back as attestations. Nobody rewrites a pipeline to try Tessra.

### Distribute through frameworks

Ship as a library and an MCP server with a one-line install. Target the default configuration of every agent framework: Claude Code, Cursor, Codex, OpenHands, and whatever comes next. Humans never install a version control system. Their agent brings it.

### One operation becomes dramatically cheaper

Git made branching free. Tessra makes parallel verified work free: N agents, N workspaces, continuous landing, one activity pack in the morning. "What did the agents do while I was away" becomes the daily ritual the way "open the PR" is today. That is the workflow that becomes culture.

### The network layer

GitHub's social objects were the fork, the pull request, and the star. Tessra's are the attestation and the intent.

A shared verification network means that when anyone verifies a dependency at a content hash in an environment, nobody else has to. Every user makes the system cheaper for every other user. Git never had a network effect at the tool level; this is one.

Agent identities carry reputation across repositories. Intents and context packs can be shared. Whether Tessra builds the hub or designs for someone else to, the protocol supports it from day one.

### Open core, monetize the network

Permissive license on the tool so frameworks embed it without a legal review. Publish the object model as a spec others can implement; an ecosystem that implements the spec is a moat. Revenue comes from the hub. This is the same split that worked for git and GitHub.

### Fear removed

Universal undo plus containment is the reflog of the AI era. The pitch to a cautious team: let agents loose, nothing is irreversible. Every action is signed, every agent holds only the capabilities it was granted, and the audit log is the history itself. See SECURITY.md.

### Proof, not claims

The AI era adopts by benchmark. Build the multi-agent coding benchmark, publish it, and show Tessra-backed agents land more changes with fewer conflicts and less human time. Owning the benchmark is a distribution channel.

### Speed as religion

Workspace creation in constant time. Context pack under a second. Verification cache hit instant. Publish the numbers and keep them. Git won on speed once and never gave it back.

### The agent manual

A compact document any model reads in two thousand tokens and uses correctly the first time. Agents that get it right on the first try keep the tool. Agents that fumble get it swapped out. This document is a product, not an afterthought.

## What would kill it

- **Requiring a migration.** Anything that makes the team change before the agent has proven value.
- **New words for old things.** Keep git's vocabulary where the semantics match. Introduce new words only for new concepts: change, snapshot, attestation, claim, pack.
- **Building the hub before the tool is loved.** GitHub came after git worked.
- **Ignoring GitHub's counter-move.** They will bolt agents onto git. The bet is that a native substrate beats patches over the wrong model, and colocation means Tessra never has to win that argument up front.

## Sequence

1. Memory. Delete the hand-written CLAUDE.md and the agents stop relearning things. Colocated in git, distributed through frameworks.
2. Standards. Turn on automated landing under a standard people wrote. Humans review exceptions.
3. Benchmark and published numbers.
4. Workflow becomes ritual: the morning activity pack and the exception queue.
5. Network layer: shared verification, agent identity, reputation.
6. Hub.

# replay

Replay is a radically simple attempt at software orchestration - to build, manage, maintain and improve software automatically.

replay is a simple loop:
- define requirements (create issues)
- create git patches that solve those issues (-> in progress)
- evaluate the strength of each patch (-> complete) and create follow ups (-> create issues)

## usage
1. empty repo
2. a README with the spec
3. the first issue: "replay should be able to read an issue and produce a patch"
4. a human (or LLM) writes the first version that can do that ONE thing
5. from that point forward, replay improves itself

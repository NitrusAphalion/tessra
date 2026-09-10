# Testing

Tests are how an agent proves a change, and coverage is how the system knows which tests prove what. In Tessra both are part of the graph, not a report that scrolls past in a CI log.

## Tests are nodes

Tests are code, so they are indexed as nodes like everything else. They merge at node granularity, test tables merge as sets, and blame, intent, and provenance apply to them. A test links to the intent it verifies, so the chain runs from spec to test to code. When a spec changes, its tests are flagged along with its code.

## Coverage is a relation

A coverage run produces attestations at node granularity: test T exercised node N at snapshot S in environment E. That relation is the coverage map, and it answers:

- Which tests cover this node. Included in every context pack and every change response.
- Which nodes have no covering test.
- Which tests to run for a change: those covering the changed nodes and their dependents.
- How coverage of a scope has changed over time.

Coverage is per node, so a clause like `coverage(changed_nodes)` is exact. A repository-wide percentage hides the gap; a node-level map points at it.

## Test selection

The frontier runs only the tests the coverage map says matter, plus a full run on a cadence that attests the whole frontier. Because attestations are cached by content hash and environment, a test whose covered nodes are unchanged is not re-run. Throughput is bounded by the number of nodes changing, not by suite length.

## Tests as proof of the change

An agent-written test can pass trivially or test the wrong thing. Standards can require proof that a test proves something.

- `tests.new.fail_on_parent`. Every new test fails on the parent snapshot and passes on the change. The test demonstrates the change rather than decorating it.
- `every_changed_node.has_covering_test`.
- `coverage(changed_nodes) >= X`.
- `new_public_nodes.have_tests`.
- `mutation_score(changed_nodes) >= X`, for scopes where the cost is justified.

These are deterministic verifiers, cached like everything else.

## Guarding the tests

An agent under pressure to pass a gate will weaken tests. Standards can:

- Forbid modifying or deleting existing tests unless approved or judged.
- Forbid skip and ignore markers.
- Route any change to a test into the exception queue in sensitive scopes.
- Require a judged clause that test changes are justified by the intent.

A test modification is structurally distinct from a test addition, so clauses can treat them differently. A change that adds tests is encouraged; a change that alters them is scrutinized.

## Flakiness

Every run is an attestation, so flakiness is measurable per test: pass rate across runs on identical content in identical environments. A flaky test is quarantined automatically as a task, excluded from gating until fixed, and its history stays visible. Standards can require N consecutive passes for a test with a flaky history before it counts again.

## Test history

"When did test T start failing" is cached bisect over attestations. "Which tests cover node N and what is their pass history" is one query. Test results are never lost in a CI log. They are part of the repository's knowledge and they are queryable forever.

## Coverage as a driver

Untested nodes are a query, and a query can become an intent. A standard can hold a coverage floor per scope. When the floor is breached, the system opens a task to add tests for the uncovered nodes and the swarm fills it. In a fully automated flow, coverage maintains itself.

## Verifier kinds for tests

- Unit and integration runners for any framework. The adapter reports per-test results.
- Coverage collectors mapped to nodes.
- The fail-on-parent checker.
- Mutation testing.
- Property-based and fuzz verifiers that generate adversarial inputs for changed nodes.

Each is a verifier producing attestations. Supporting a new framework means adding an adapter, not changing the model.

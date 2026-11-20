# Fanout agent grid
- One project per workers tab; never mix projects or owners, or put 2+ workers in the CoS tab.
- Label each pane with the worker name. Moving or swapping a pane while its agent runs is allowed — the move keeps the pane, its terminal, the agent process and its session, and a tool call started before the move still completes (issue #192). The grid still defers a `working` pane (`[grid] deferred_statuses = ["working"]`), so the allowance is a herdr capability, not a licence for the repair to move a busy pane; still avoid moving panes during `--wait`.

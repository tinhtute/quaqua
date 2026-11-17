-- T44: the pane the engine placed the agent in at spawn. Reconcile and cleanup
-- close only a pane recorded here.
ALTER TABLE agent ADD COLUMN pane_id TEXT;

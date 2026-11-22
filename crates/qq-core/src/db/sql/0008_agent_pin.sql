-- T52b: the harness, model and effort pinned on the agent row at spawn.
ALTER TABLE agent ADD COLUMN harness TEXT;
ALTER TABLE agent ADD COLUMN model TEXT;
ALTER TABLE agent ADD COLUMN effort TEXT;

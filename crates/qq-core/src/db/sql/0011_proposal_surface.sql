-- T63b: the surface a proposal would change (rule, skill, config or protocol).
-- NULL for a proposal whose change names none.
ALTER TABLE proposal ADD COLUMN surface TEXT CHECK (surface IN ('rule', 'skill', 'config', 'protocol'));

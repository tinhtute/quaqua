# Encode Lessons in Structure

When you catch yourself writing the same instruction a second time, or notice a recurring correction, encode the rule as a lint, metadata flag, runtime check, or script instead of adding more text.

## Why

Text instructions in prompts and documentation are easily forgotten, diluted, or overlooked over time. Structural constraints (compilers, linters, pre-commit hooks, tests, schema validations) enforce invariants mechanically and reliably.

## When

You catch yourself writing the same instruction a second time, or notice a recurring correction.

## Rule

- Encode the rule as a lint, metadata flag, runtime check, or script instead of more prose.
- If the fix is structural, only use the structural fix. The instruction is the symptom.
- If the lesson requires judgment and cannot be fully automated, make the instruction prominent, place it at the point of action, and add a concrete example of the failure mode.

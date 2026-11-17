---
name: prototype
when: A throwaway sketch to make a design or behavioral decision cheaply, or to settle an empirical fork by observing it instead of asking the human.
---

## Orchestrator

1. You own the design decision, not the production code. The prototype is a throwaway instrument; the real build follows `feature`.
2. This is the one playbook where the smallest-change bias of principle-laziness-protocol and the verification bar invert: speed over polish, code quality does not matter, no planning. The rigor is in picking the right design cheaply. Propose variations the user didn't ask for, throw an approach away, and try another.
3. Frame the empirical fork or design choice clearly in the brief. If there is no decision to settle, skip prototyping and route directly to `feature`.

## Worker

1. Scope the decision the prototype exists to make: layout, interaction, density, or for an empirical fork, observable behavior, timing, or approach. If no decision is at stake, stop and route to `feature`.
2. Gather references when the design space is open. Search for prior art, outline themes and layouts, and let the coordinator or user confirm direction before building. Skip when direction is already set.
3. Build throwaway in an isolated scratch directory, separate from production source. For a visual decision, use the lightest markup and styles that render the idea. For a behavioral or timing decision, write the smallest standalone script that exercises the question. No production framework, no tests, no abstractions.
4. When comparing alternatives, build them behind one switcher (flags, buttons, or inputs), with each variant clearly labeled per principle-exhaust-the-design-space.
5. Verify on the matching surface. For visual decisions, run headless only per rules/_all/no-gui-driving.md (isolated/headless display only when explicitly authorized in the brief, never driving the real display); for behavioral or timing decisions, observe the thing you are deciding by logging execution time, printing output, or watching the render. The observation is the test here, not an assertion.
6. Present the variants explored, evidence, tradeoffs, and a recommendation. State plainly that the prototype is throwaway.
7. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** the variants explored, the evidence (headless capture for a visual decision, the observed output or timing for a behavioral one), tradeoffs, your recommendation, and the scratch path. Say plainly that the prototype is throwaway.

---
name: "speckit-specify"
description: "Create or update the feature specification from a natural language feature description."
compatibility: "Requires only the spec template at reference/spec-template.md (relative to this skill's directory)"
metadata:
  author: "github-spec-kit"
  source: "templates/commands/specify.md"
---

## User Input

```text
$ARGUMENTS
```

You **MUST** consider the user input before proceeding (if not empty).

## Outline

The text the user typed after `/speckit-specify` in the triggering message **is** the feature description. Assume you always have it available in this conversation even if `$ARGUMENTS` appears literally below. The feature description may be inline text or a path to a document file (e.g., a `.md` file) — if it is a document file path, read that file and use its content as the feature description. Do not ask the user to repeat it unless they provided an empty command.

Given that feature description, do this:

- **Determine the short name** (2-4 words) for the feature:
  - If the feature description already includes a feature title/name, use it as the short name (converting to kebab-case if needed)
  - Otherwise, analyze the feature description and extract the most meaningful keywords
  - Create a 2-4 word short name that captures the essence of the feature
  - Use action-noun format when possible (e.g., "add-user-auth", "fix-payment-bug")
  - Preserve technical terms and acronyms (OAuth2, API, JWT, etc.)
  - Keep it concise but descriptive enough to understand the feature at a glance
  - Examples: - "I want to add user authentication" → "user-auth" - "Implement OAuth2 integration for the API" → "oauth2-api-integration" - "Create a dashboard for analytics" → "analytics-dashboard" - "Fix payment processing timeout bug" → "fix-payment-timeout"
    **Create the spec feature directory**:

All files are generated into `SPECIFY_FEATURE_DIRECTORY` — the feature directory for this spec. Nothing is created directly in the current folder.

**Resolution order for** **`SPECIFY_FEATURE_DIRECTORY`**:

- If the user explicitly provided `SPECIFY_FEATURE_DIRECTORY` (e.g., via environment variable, argument, or configuration), use it as-is
- Otherwise, ask the user for the target directory before creating anything
- If the user provides only a parent location, suggest `<NNN>-<short-name>` as the directory name (next available 3-digit number after scanning existing feature directories in that parent), e.g., `003-user-auth`

**Create the directory and spec file**:

- `mkdir -p SPECIFY_FEATURE_DIRECTORY`
- Copy the spec template from `reference/spec-template.md` (relative to this skill's directory) to `SPECIFY_FEATURE_DIRECTORY/spec.md` as the starting point
- Set `SPEC_FILE` to `SPECIFY_FEATURE_DIRECTORY/spec.md`

**IMPORTANT**:

- Create one feature per `/speckit-specify` invocation by default; if the user's description provides multiple distinct features, write all of them into the same `SPECIFY_FEATURE_DIRECTORY/spec.md`, repeating the template structure for each feature

* Load the spec template file to understand required sections.
* Follow this execution flow:
  1. Parse user description from arguments
     If empty: ERROR "No feature description provided"
  2. Extract key concepts from description
     Identify: actors, actions, data, constraints
  3. For unclear aspects:
     - Make informed guesses based on context and industry standards
     - Only mark with \[NEEDS CLARIFICATION: specific question] if:
       - The choice significantly impacts feature scope or user experience
       - Multiple reasonable interpretations exist with different implications
       - No reasonable default exists
     - **LIMIT: Maximum 3 \[NEEDS CLARIFICATION] markers total**
     - Prioritize clarifications by impact: scope > security/privacy > user experience > technical details
  4. Fill User Scenarios & Testing section
     If no clear user flow: ERROR "Cannot determine user scenarios"
  5. Generate Functional Requirements
     Each requirement must be testable
     Use reasonable defaults for unspecified details (document assumptions in Assumptions section)
  6. Define Success Criteria
     Create measurable, technology-agnostic outcomes
     Include both quantitative metrics (time, performance, volume) and qualitative measures (user satisfaction, task completion)
     Each criterion must be verifiable without implementation details
  7. Identify Key Entities (if data involved)
  8. Return: SUCCESS (spec ready for planning)
* Write the specification to SPEC_FILE using the template structure, replacing placeholders with concrete details derived from the feature description (arguments) while preserving section order and headings.
* **Specification Quality Validation**: After writing the initial spec, validate it against quality criteria:

  a. **Validation Criteria**: The spec must satisfy all of the following:

  Content Quality:
  - No implementation details (languages, frameworks, APIs)
  - Focused on user value and business needs
  - Written for non-technical stakeholders
  - All mandatory sections completed

  Requirement Completeness:
  - No \[NEEDS CLARIFICATION] markers remain
  - Requirements are testable and unambiguous
  - Success criteria are measurable
  - Success criteria are technology-agnostic (no implementation details)
  - All acceptance scenarios are defined
  - Edge cases are identified
  - Scope is clearly bounded
  - Dependencies and assumptions identified

  Feature Readiness:
  - All functional requirements have clear acceptance criteria
  - User scenarios cover primary flows
  - Feature meets measurable outcomes defined in Success Criteria
  - No implementation details leak into specification

  b. **Run Validation Check**: Review the spec against each criterion:
  - For each item, determine if it passes or fails
  - Document specific issues found (quote relevant spec sections)
    c. **Handle Validation Results**:
  - **If all items pass**: Proceed to the Completion Report section
  - **If items fail (excluding \[NEEDS CLARIFICATION])**:
    1. List the failing items and specific issues
    2. Update the spec to address each issue
    3. Re-run validation until all items pass (max 3 iterations)
    4. If still failing after 3 iterations, document remaining issues and warn user
  - **If \[NEEDS CLARIFICATION] markers remain**:
    1. Extract all \[NEEDS CLARIFICATION: ...] markers from the spec
    2. **LIMIT CHECK**: If more than 3 markers exist, keep only the 3 most critical (by scope/security/UX impact) and make informed guesses for the rest
    3. For each clarification needed (max 3), present options to user in this format:

       ```markdown
       ## Question [N]: [Topic]

       **Context**: [Quote relevant spec section]

       **What we need to know**: [Specific question from NEEDS CLARIFICATION marker]

       **Suggested Answers**:

       | Option | Answer                    | Implications                          |
       | ------ | ------------------------- | ------------------------------------- |
       | A      | [First suggested answer]  | [What this means for the feature]     |
       | B      | [Second suggested answer] | [What this means for the feature]     |
       | C      | [Third suggested answer]  | [What this means for the feature]     |
       | Custom | Provide your own answer   | [Explain how to provide custom input] |

       **Your choice**: _[Wait for user response]_
       ```

    4. **CRITICAL - Table Formatting**: Ensure markdown tables are properly formatted:
       - Use consistent spacing with pipes aligned
       - Each cell should have spaces around content: `| Content |` not `|Content|`
       - Header separator must have at least 3 dashes: `|--------|`
       - Test that the table renders correctly in markdown preview
    5. Number questions sequentially (Q1, Q2, Q3 - max 3 total)
    6. Present all questions together before waiting for responses
    7. Wait for user to respond with their choices for all questions (e.g., "Q1: A, Q2: Custom - \[details], Q3: B")
    8. Update the spec by replacing each \[NEEDS CLARIFICATION] marker with the user's selected or provided answer
    9. Re-run validation after all clarifications are resolved

* **Independent Spec Review**: After the specification passes validation, dispatch a spec reviewer subagent using the prompt template at `reference/spec-document-reviewer-prompt.md` (relative to this skill's directory) with `[SPEC_FILE_PATH]` set to the spec's path:
  - Write the reviewer's output (Status, Issues, Recommendations) into the `## Need Clarification` section of the spec, below the feature description
  - For each issue: resolve it by updating the spec where the answer is clear, or ask the user using the question format above; remove the entry once resolved
  - Record recommendations as advisory entries
  - Re-dispatch the reviewer after resolving issues (max 3 iterations), then proceed to the Completion Report

## Completion Report

Report completion to the user with:

- `SPECIFY_FEATURE_DIRECTORY` — the feature directory path
- `SPEC_FILE` — the spec file path
- Validation and spec-review results summary
- Readiness for the next phase (`/speckit-clarify` or `/speckit-plan`)

## Quick Guidelines

- Focus on **WHAT** users need and **WHY**.
- Avoid HOW to implement (no tech stack, APIs, code structure).
- Written for business stakeholders, not developers.
- DO NOT create any checklists that are embedded in the spec. That will be a separate command.

### Section Requirements

- **Mandatory sections**: Must be completed for every feature
- **Optional sections**: Include only when relevant to the feature
- When a section doesn't apply, remove it entirely (don't leave as "N/A")

### For AI Generation

When creating this spec from a user prompt:

1. **Make informed guesses**: Use context, industry standards, and common patterns to fill gaps
2. **Document assumptions**: Record reasonable defaults in the Assumptions section
3. **Limit clarifications**: Maximum 3 \[NEEDS CLARIFICATION] markers - use only for critical decisions that:
   - Significantly impact feature scope or user experience
   - Have multiple reasonable interpretations with different implications
   - Lack any reasonable default
4. **Prioritize clarifications**: scope > security/privacy > user experience > technical details
5. **Think like a tester**: Every vague requirement should fail the "testable and unambiguous" validation criterion
6. **Common areas needing clarification** (only if no reasonable default exists):
   - Feature scope and boundaries (include/exclude specific use cases)
   - User types and permissions (if multiple conflicting interpretations possible)
   - Security/compliance requirements (when legally/financially significant)

**Examples of reasonable defaults** (don't ask about these):

- Data retention: Industry-standard practices for the domain
- Performance targets: Standard web/mobile app expectations unless specified
- Error handling: User-friendly messages with appropriate fallbacks
- Authentication method: Standard session-based or OAuth2 for web apps
- Integration patterns: Use project-appropriate patterns (REST/GraphQL for web services, function calls for libraries, CLI args for tools, etc.)

### Success Criteria Guidelines

Success criteria must be:

1. **Measurable**: Include specific metrics (time, percentage, count, rate)
2. **Technology-agnostic**: No mention of frameworks, languages, databases, or tools
3. **User-focused**: Describe outcomes from user/business perspective, not system internals
4. **Verifiable**: Can be tested/validated without knowing implementation details

**Good examples**:

- "Users can complete checkout in under 3 minutes"
- "System supports 10,000 concurrent users"
- "95% of searches return results in under 1 second"
- "Task completion rate improves by 40%"

**Bad examples** (implementation-focused):

- "API response time is under 200ms" (too technical, use "Users see results instantly")
- "Database can handle 1000 TPS" (implementation detail, use user-facing metric)
- "React components render efficiently" (framework-specific)
- "Redis cache hit rate above 80%" (technology-specific)

## Done When

- [ ] Specification written to `SPEC_FILE` and validated against the quality criteria
- [ ] Independent spec review completed, output recorded in the Need Clarification section, and no unresolved issues remain
- [ ] Completion reported to user with feature directory, spec file path, and validation results

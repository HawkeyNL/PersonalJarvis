# Input Validation Standard

Parse and validate at every trust boundary, then construct trusted domain types.

Layers:
1. transport and payload limits;
2. syntax/schema validation;
3. semantic and cross-field validation;
4. authorization;
5. current-state/business validation;
6. policy/risk validation;
7. external adapter validation.

Free text must never directly become SQL, shell, filesystem paths, URLs, permissions, executable code or broker orders.

Use canonical InstrumentIds, Decimal money types, strict URL parsing/allowlists, upload limits and safe path handling.

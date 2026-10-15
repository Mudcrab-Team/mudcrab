# Record comparison primitives

Run the synthetic controls with Python 3.10 or newer:

```text
python -B tools/record_comparison/test_compare_values.py
python -B -O tools/record_comparison/test_compare_values.py
```

`compare_values.py` compares decoded values and resolves ordered children using
caller-supplied mappings. Missing values, SQL NULL, zero links and nonfinite
numbers remain explicit unless the caller supplies a witnessed classification.
Numeric comparisons use exact finite values. Boolean values retain their type.

`Children` validates owner, parent and ordinal identities. `Ledger` requires
every declared reference cell to enter one classification and reports types with
no observed reference cells explicitly. Candidate children have a separate
denominator. Callers must independently count input rows and columns to prove
that their traversal visited the entire reference population.

Native format facts and reference field mappings belong in external audit
evidence. An `explained` result means a supplied interpretation accounts for a
difference; it does not mean the two stored values match. These primitives do
not establish record identity compatibility, gameplay or rendering parity.

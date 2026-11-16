# Saved fuzz inputs

`fuzz_hang_check.um` is seed 3 from the backend differential harness
(`compiler/tests/backend_diff.rs`). `umay check` does not terminate on it,
while the reference interpreter produces output immediately. Kept as a
reproduction for the compiler hang; it is not run by the test suite.

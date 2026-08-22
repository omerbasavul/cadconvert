using Xunit;

// Every test here drives the same native library through P/Invoke, and several
// of them convert the same sample. Running whole classes concurrently would
// make any timing or memory reading meaningless and would put two conversions
// in one temporary directory. Concurrency is worth testing, so it is tested
// deliberately — inside ConcurrencyTests — rather than imposed by the runner.
[assembly: CollectionBehavior(DisableTestParallelization = true)]

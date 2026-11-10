/*
 * Test library that imports a symbol the bionic table does not provide, used
 * to verify the `UNSUPPORTED_NATIVE_SYMBOL` policy and the LazyTrap mode.
 */

extern int acos_definitely_missing_symbol(void);

int testmissing_call(void) { return acos_definitely_missing_symbol(); }

int testmissing_present(void) { return 1; }

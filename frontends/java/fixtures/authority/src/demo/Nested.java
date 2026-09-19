package demo;

import java.util.function.IntSupplier;

/** Hosts an argument-nested call and an anonymous-class call for owner attribution. */
public final class Nested {
	private Nested() {}

	/** The nested argument call must attribute to this declared method, not its callee. */
	public String composed(int value) {
		return Helper.render(String.valueOf(value));
	}

	/** The anonymous-body call must attribute to this declared method of the named type. */
	public IntSupplier delayed(int base) {
		return new IntSupplier() {
			@Override
			public int getAsInt() {
				return Integer.bitCount(base);
			}
		};
	}
}


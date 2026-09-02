package demo;

/** Calls a declaration from another compilation unit in the same module. */
public final class Cafe {
	/** Resolves the exact overload through javac attribution. */
	public String brew(int cups) {
		return "😀" + Helper.render(cups);
	}
}

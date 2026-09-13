package demo;

import java.io.IOException;

/** Calls a declaration from another compilation unit in the same module. */
public record Cafe(String left, int right) {
	/** Resolves the exact overload through javac attribution. */
	public String brew(int cups) {
		return "😀" + Helper.render(cups);
	}

	@Deprecated
	public void audited() throws IOException { }

}


package demo;

/** A separately compiled target for the cross-file resolution proof. */
public final class Helper {
	private Helper() {}

	/** Renders a value without depending on source-text recovery. */
	public static String render(int value) {
		return "v:" + value;
	}

	/** Separates overload identity from its spelling at the call site. */
	public static String render(String value) {
		return value;
	}
}

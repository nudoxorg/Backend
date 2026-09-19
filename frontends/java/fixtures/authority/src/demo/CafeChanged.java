package demo;

/** A source mutation that must select the string overload instead. */
public final class CafeChanged {
	/** Changes the argument type without changing the callee spelling. */
	public String brew(String cups) {
		return Helper.render(cups);
	}
}


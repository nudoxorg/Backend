package nudox.oracle;

import java.util.ArrayDeque;
import java.util.Deque;

/**
 * A minimal streaming JSON writer.
 *
 * The oracle deliberately has zero external dependencies (it is compiled with
 * a bare {@code javac} at runtime), so JSON emission is hand-rolled here.
 * Commas are inserted automatically; strings are escaped per RFC 8259.
 */
final class Json {
	private final StringBuilder out = new StringBuilder(1 << 20);
	/** One slot per open container: {@code true} once it has an element. */
	private final Deque<boolean[]> containers = new ArrayDeque<>();
	/** Set between {@link #name} and the value it introduces. */
	private boolean afterName = false;

	/** Insert a separating comma if the current container already has items. */
	private void prepareValue() {
		if (afterName) {
			afterName = false;
			return;
		}
		boolean[] container = containers.peek();
		if (container != null) {
			if (container[0]) {
				out.append(',');
			}
			container[0] = true;
		}
	}

	void beginObject() {
		prepareValue();
		out.append('{');
		containers.push(new boolean[1]);
	}

	void endObject() {
		containers.pop();
		out.append('}');
	}

	void beginArray() {
		prepareValue();
		out.append('[');
		containers.push(new boolean[1]);
	}

	void endArray() {
		containers.pop();
		out.append(']');
	}

	/** Write an object key; the next write is its value. */
	void name(String key) {
		boolean[] container = containers.peek();
		if (container != null) {
			if (container[0]) {
				out.append(',');
			}
			container[0] = true;
		}
		string(key);
		out.append(':');
		afterName = true;
	}

	void value(String s) {
		prepareValue();
		if (s == null) {
			out.append("null");
		} else {
			string(s);
		}
	}

	void value(long v) {
		prepareValue();
		out.append(v);
	}

	void value(double v) {
		prepareValue();
		if (Double.isFinite(v)) {
			out.append(v);
		} else {
			// JSON has no NaN/Infinity; degrade to a string.
			string(Double.toString(v));
		}
	}

	void value(boolean v) {
		prepareValue();
		out.append(v);
	}

	void nullValue() {
		prepareValue();
		out.append("null");
	}

	private void string(String s) {
		out.append('"');
		for (int i = 0; i < s.length(); i++) {
			char c = s.charAt(i);
			switch (c) {
				case '"' -> out.append("\\\"");
				case '\\' -> out.append("\\\\");
				case '\n' -> out.append("\\n");
				case '\r' -> out.append("\\r");
				case '\t' -> out.append("\\t");
				case '\b' -> out.append("\\b");
				case '\f' -> out.append("\\f");
				default -> {
					if (c < 0x20) {
						out.append(String.format("\\u%04x", (int) c));
					} else {
						out.append(c);
					}
				}
			}
		}
		out.append('"');
	}

	@Override
	public String toString() {
		return out.toString();
	}
}

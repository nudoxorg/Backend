/**
 * Runs a release-pinned JavacTask and produces one immutable authority image.
 * It retains exact compiler diagnostics and refuses unsupported profile releases.
 * The extraction path contains no scanner, JSON transport, or source-text recovery.
 */
package nudox.oracle;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

import javax.tools.Diagnostic;
import javax.tools.DiagnosticCollector;
import javax.tools.JavaCompiler;
import javax.tools.JavaFileObject;
import javax.tools.StandardJavaFileManager;
import javax.tools.ToolProvider;

import com.sun.source.tree.CompilationUnitTree;
import com.sun.source.util.JavacTask;

/** Runs source files through javac's attributed-tree API into the binary authority image. */
public final class CompilerExtractor {
	private CompilerExtractor() {}

	public static void main(String[] arguments) throws IOException {
		Invocation invocation = Invocation.parse(arguments);
		JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
		if (compiler == null) {
			throw new IllegalStateException("the running Java installation has no system compiler");
		}
		DiagnosticCollector<JavaFileObject> diagnostics = new DiagnosticCollector<>();
		try (StandardJavaFileManager files = compiler.getStandardFileManager(
			diagnostics, null, StandardCharsets.UTF_8
		)) {
			Iterable<? extends JavaFileObject> sources = files.getJavaFileObjectsFromStrings(
				invocation.sources
			);
			JavacTask task = (JavacTask) compiler.getTask(
				null, files, diagnostics,
				List.of("-proc:none", "-XDkeepComments", "--release", Integer.toString(invocation.release)), null, sources
			);
			Iterable<? extends CompilationUnitTree> units = task.parse();
			task.analyze();

			throwIfCompilationFailed(diagnostics.getDiagnostics());
			AuthorityImage.write(task, units, invocation.release, invocation.sourceBinding, invocation.output);
		}
	}

	private static void throwIfCompilationFailed(
		List<Diagnostic<? extends JavaFileObject>> diagnostics
	) throws CompilationFailure {
		List<Diagnostic<? extends JavaFileObject>> errors = diagnostics.stream()
			.filter(diagnostic -> diagnostic.getKind() == Diagnostic.Kind.ERROR)
			.toList();
		if (!errors.isEmpty()) {
			throw new CompilationFailure(errors);
		}
	}

	/** Retains every javac diagnostic instead of dropping all but the first. */
	private static final class CompilationFailure extends IOException {
		private final List<Diagnostic<? extends JavaFileObject>> diagnostics;

		CompilationFailure(List<Diagnostic<? extends JavaFileObject>> diagnostics) {
			super(render(diagnostics));
			this.diagnostics = List.copyOf(diagnostics);
		}

		private static String render(List<Diagnostic<? extends JavaFileObject>> diagnostics) {
			return diagnostics.stream()
				.map(Diagnostic::toString)
				.collect(java.util.stream.Collectors.joining(System.lineSeparator()));
		}
	}

	private static final class Invocation {
		private final Path output;
		private final int release;
		private final Path sourceBinding;
		private final List<String> sources;

		private Invocation(Path output, int release, Path sourceBinding, List<String> sources) {
			this.output = output;
			this.release = release;
			this.sourceBinding = sourceBinding;
			this.sources = sources;
		}

		private static Invocation parse(String[] arguments) {
			if (arguments.length < 7
				|| !arguments[0].equals("--release")
				|| !arguments[2].equals("--outfile")
				|| !arguments[4].equals("--source-binding")) {
				throw new IllegalArgumentException(
					"usage: CompilerExtractor --release <release> --outfile <path> --source-binding <source> <source>..."
				);
			}
			List<String> sources = new ArrayList<>();
			for (int index = 6; index < arguments.length; index++) {
				sources.add(arguments[index]);
			}
			return new Invocation(Path.of(arguments[3]), release(arguments[1]), Path.of(arguments[5]), List.copyOf(sources));
		}

		private static int release(String argument) {
			return switch (argument) {
				case "8" -> 8;
				case "11" -> 11;
				case "17" -> 17;
				case "21" -> 21;
				case "25" -> 25;
				default -> throw new IllegalArgumentException("unsupported Java language profile release: " + argument);
			};
		}
	}
}

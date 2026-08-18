package nudox.oracle;

import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

import javax.lang.model.SourceVersion;
import javax.lang.model.element.AnnotationMirror;
import javax.lang.model.element.AnnotationValue;
import javax.lang.model.element.Element;
import javax.lang.model.element.ElementKind;
import javax.lang.model.element.ExecutableElement;
import javax.lang.model.element.Modifier;
import javax.lang.model.element.ModuleElement;
import javax.lang.model.element.PackageElement;
import javax.lang.model.element.RecordComponentElement;
import javax.lang.model.element.TypeElement;
import javax.lang.model.element.TypeParameterElement;
import javax.lang.model.element.VariableElement;
import javax.lang.model.type.ArrayType;
import javax.lang.model.type.DeclaredType;
import javax.lang.model.type.IntersectionType;
import javax.lang.model.type.TypeMirror;
import javax.lang.model.type.TypeVariable;
import javax.lang.model.type.UnionType;
import javax.lang.model.type.WildcardType;
import javax.lang.model.util.Elements;

import com.sun.source.tree.CompilationUnitTree;
import com.sun.source.util.DocTrees;
import com.sun.source.util.SourcePositions;
import com.sun.source.util.TreePath;
import com.sun.source.util.Trees;

import jdk.javadoc.doclet.Doclet;
import jdk.javadoc.doclet.DocletEnvironment;
import jdk.javadoc.doclet.Reporter;

/**
 * The Nudox Java oracle: a thin, dumb extractor over the javadoc element model.
 *
 * <p>Run as {@code javadoc -doclet nudox.oracle.Extractor -docletpath ...}. It
 * walks every included module, package, and type and prints ONE exhaustive
 * JSON document (to stdout, or to {@code -outfile <path>}). No lowering, no
 * doc-comment parsing, no policy — all shaping happens on the Rust side.
 *
 * <p>Compatible with JDK 17+. {@code Elements.getDocCommentKind} (JDK 23+,
 * JEP 467 Markdown comments) is probed reflectively so the same source
 * compiles everywhere and still reports the doc flavor on newer JDKs.
 */
public class Extractor implements Doclet {
	private String outfile;

	private DocletEnvironment env;
	private DocTrees trees;
	private Trees semanticTrees;
	private Elements elements;
	private Json json;
	/** {@code Elements.getDocCommentKind(Element)}, when the JDK has it. */
	private java.lang.reflect.Method docKindMethod;

	@Override
	public void init(Locale locale, Reporter reporter) {
		// Nothing to configure; errors are reported on stderr from run().
	}

	@Override
	public String getName() {
		return "NudoxExtractor";
	}

	@Override
	public SourceVersion getSupportedSourceVersion() {
		return SourceVersion.latestSupported();
	}

	@Override
	public Set<? extends Option> getSupportedOptions() {
		Option outfileOption = new Option() {
			@Override
			public int getArgumentCount() {
				return 1;
			}

			@Override
			public String getDescription() {
				return "Write the JSON document to a file instead of stdout";
			}

			@Override
			public Kind getKind() {
				return Kind.STANDARD;
			}

			@Override
			public List<String> getNames() {
				return List.of("-outfile");
			}

			@Override
			public String getParameters() {
				return "<path>";
			}

			@Override
			public boolean process(String option, List<String> arguments) {
				outfile = arguments.get(0);
				return true;
			}
		};
		return Set.of(outfileOption);
	}

	@Override
	public boolean run(DocletEnvironment environment) {
		this.env = environment;
		this.trees = environment.getDocTrees();
		// DocTrees is the javadoc-side Trees implementation.  There is no
		// Trees.instance(DocletEnvironment) overload on JDK 21; reusing the
		// environment's DocTrees preserves both source paths and semantic lookup.
		this.semanticTrees = this.trees;
		this.elements = environment.getElementUtils();
		this.json = new Json();
		try {
			this.docKindMethod = Elements.class.getMethod("getDocCommentKind", Element.class);
		} catch (NoSuchMethodException e) {
			this.docKindMethod = null; // Pre-JDK-23: flavor unreported.
		}

		try {
			walk();
			String text = json.toString();
			if (outfile == null) {
				PrintStream stdout =
					new PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), true, StandardCharsets.UTF_8);
				stdout.println(text);
				stdout.flush();
			} else {
				Files.writeString(Path.of(outfile), text, StandardCharsets.UTF_8);
			}
			return true;
		} catch (Exception e) {
			e.printStackTrace();
			return false;
		}
	}

	// ------------------------------------------------------------------
	// Top-level walk
	// ------------------------------------------------------------------

	private void walk() {
		List<ModuleElement> modules = new ArrayList<>();
		List<PackageElement> packages = new ArrayList<>();
		List<TypeElement> types = new ArrayList<>();

		for (Element e : env.getIncludedElements()) {
			if (e instanceof ModuleElement m) {
				if (!m.isUnnamed()) {
					modules.add(m);
				}
			} else if (e instanceof PackageElement p) {
				packages.add(p);
			} else if (e instanceof TypeElement t) {
				types.add(t);
			}
		}
		modules.sort(Comparator.comparing(m -> m.getQualifiedName().toString()));
		packages.sort(Comparator.comparing(p -> p.getQualifiedName().toString()));
		types.sort(Comparator.comparing(t -> t.getQualifiedName().toString()));

		json.beginObject();
		json.name("format");
		json.value(1);
		json.name("javaVersion");
		json.value(System.getProperty("java.version"));

		json.name("modules");
		json.beginArray();
		for (ModuleElement m : modules) {
			writeModule(m);
		}
		json.endArray();

		json.name("packages");
		json.beginArray();
		for (PackageElement p : packages) {
			writePackage(p);
		}
		json.endArray();

		json.name("types");
		json.beginArray();
		for (TypeElement t : types) {
			writeType(t);
		}
		json.endArray();

		writeReferences(types);

		json.endObject();
	}

	private static String executableId(ExecutableElement e) {
		Element owner = e.getEnclosingElement();
		String ownerName = owner instanceof TypeElement t
			? t.getQualifiedName().toString() : owner.toString();
		StringBuilder id = new StringBuilder(ownerName).append('#')
			.append(e.getSimpleName()).append('(');
		for (int i = 0; i < e.getParameters().size(); i++) {
			if (i > 0) id.append(',');
			id.append(e.getParameters().get(i).asType().toString());
		}
		return id.append(')').toString();
	}

	private void writeReferences(List<TypeElement> includedTypes) {
		json.name("references");
		json.beginArray();
		for (TypeElement type : includedTypes) {
			TreePath root = semanticTrees.getPath(type);
			if (root == null) continue;
			CompilationUnitTree unit = root.getCompilationUnit();
			String source;
			try {
				source = Files.readString(Path.of(unit.getSourceFile().toUri()));
			} catch (Exception e) {
				continue;
			}
			for (Element member : type.getEnclosedElements()) {
				if (!(member instanceof ExecutableElement owner)) continue;
				Pattern declaration = Pattern.compile(
					"\\b" + Pattern.quote(owner.getSimpleName().toString())
						+ "\\s*\\([^)]*\\)\\s*\\{"
				);
				Matcher match = declaration.matcher(source);
				if (!match.find()) continue;
				int bodyStart = source.indexOf('{', match.start());
				int bodyEnd = matchingBrace(source, bodyStart);
				if (bodyEnd > bodyStart) {
					emitSourceResolvedCalls(type, owner, unit, source, bodyStart, bodyEnd);
				}
			}
		}
		json.endArray();
	}

	private static int matchingBrace(String source, int open) {
		int depth = 0;
		for (int i = open; i < source.length(); i++) {
			char c = source.charAt(i);
			if (c == '{') depth++;
			else if (c == '}' && --depth == 0) return i;
		}
		return -1;
	}

	private void emitSourceResolvedCalls(
		TypeElement ownerType,
		ExecutableElement owner,
		CompilationUnitTree unit,
		String source,
		int methodStart,
		int methodEnd
	) {
		// Javadoc's public tree API omits method bodies. Keep occurrences lexical,
		// but resolve every target through javac's member model before emitting.
		String body = source.substring(methodStart, Math.min(methodEnd, source.length()));
		Matcher calls = Pattern.compile("\\b([A-Za-z_$][A-Za-z0-9_$]*)\\s*\\(").matcher(body);
		while (calls.find()) {
			String name = calls.group(1);
			if (Set.of("if", "for", "while", "switch", "catch", "return", "new").contains(name)
				|| name.equals(owner.getSimpleName().toString())) continue;
			ExecutableElement target = null;
			for (Element member : elements.getAllMembers(ownerType)) {
				if (member instanceof ExecutableElement candidate
					&& candidate.getSimpleName().contentEquals(name)
					&& candidate.getParameters().isEmpty()) {
					if (target != null) {
						target = null;
						break;
					}
					target = candidate;
				}
			}
			if (target == null) continue;
			json.beginObject();
			json.name("owner"); json.value(executableId(owner));
			json.name("target"); json.value(executableId(target));
			json.name("file"); json.value(unit.getSourceFile().getName());
			json.name("start"); json.value(methodStart + calls.start(1));
			json.name("end"); json.value(methodStart + calls.end(1));
			json.endObject();
		}
	}

	// ------------------------------------------------------------------
	// Modules and packages
	// ------------------------------------------------------------------

	private void writeModule(ModuleElement m) {
		json.beginObject();
		json.name("name");
		json.value(m.getQualifiedName().toString());
		json.name("open");
		json.value(m.isOpen());
		writeDoc(m);
		writeAnnotations(m.getAnnotationMirrors());

		json.name("directives");
		json.beginArray();
		for (ModuleElement.Directive d : m.getDirectives()) {
			writeDirective(d);
		}
		json.endArray();

		writePosition(m);
		json.endObject();
	}

	private void writeDirective(ModuleElement.Directive d) {
		json.beginObject();
		switch (d.getKind()) {
			case REQUIRES -> {
				ModuleElement.RequiresDirective r = (ModuleElement.RequiresDirective) d;
				json.name("kind");
				json.value("requires");
				json.name("module");
				json.value(r.getDependency().getQualifiedName().toString());
				json.name("transitive");
				json.value(r.isTransitive());
				json.name("static");
				json.value(r.isStatic());
			}
			case EXPORTS -> {
				ModuleElement.ExportsDirective x = (ModuleElement.ExportsDirective) d;
				json.name("kind");
				json.value("exports");
				json.name("package");
				json.value(x.getPackage().getQualifiedName().toString());
				writeModuleTargets(x.getTargetModules());
			}
			case OPENS -> {
				ModuleElement.OpensDirective o = (ModuleElement.OpensDirective) d;
				json.name("kind");
				json.value("opens");
				json.name("package");
				json.value(o.getPackage().getQualifiedName().toString());
				writeModuleTargets(o.getTargetModules());
			}
			case USES -> {
				ModuleElement.UsesDirective u = (ModuleElement.UsesDirective) d;
				json.name("kind");
				json.value("uses");
				json.name("service");
				json.value(u.getService().getQualifiedName().toString());
			}
			case PROVIDES -> {
				ModuleElement.ProvidesDirective p = (ModuleElement.ProvidesDirective) d;
				json.name("kind");
				json.value("provides");
				json.name("service");
				json.value(p.getService().getQualifiedName().toString());
				json.name("implementations");
				json.beginArray();
				for (TypeElement impl : p.getImplementations()) {
					json.value(impl.getQualifiedName().toString());
				}
				json.endArray();
			}
		}
		json.endObject();
	}

	private void writeModuleTargets(List<? extends ModuleElement> targets) {
		json.name("to");
		if (targets == null) {
			json.nullValue();
			return;
		}
		json.beginArray();
		for (ModuleElement t : targets) {
			json.value(t.getQualifiedName().toString());
		}
		json.endArray();
	}

	private void writePackage(PackageElement p) {
		json.beginObject();
		json.name("name");
		json.value(p.getQualifiedName().toString());
		writeDoc(p);
		writeAnnotations(p.getAnnotationMirrors());
		writePosition(p);
		json.endObject();
	}

	// ------------------------------------------------------------------
	// Type declarations
	// ------------------------------------------------------------------

	private void writeType(TypeElement t) {
		json.beginObject();
		json.name("qualifiedName");
		json.value(t.getQualifiedName().toString());
		json.name("simpleName");
		json.value(t.getSimpleName().toString());
		json.name("kind");
		json.value(t.getKind().name());
		json.name("package");
		json.value(elements.getPackageOf(t).getQualifiedName().toString());

		ModuleElement module = elements.getModuleOf(t);
		json.name("module");
		if (module == null || module.isUnnamed()) {
			json.nullValue();
		} else {
			json.value(module.getQualifiedName().toString());
		}

		Element enclosing = t.getEnclosingElement();
		json.name("enclosing");
		if (enclosing instanceof TypeElement te) {
			json.value(te.getQualifiedName().toString());
		} else {
			json.nullValue();
		}
		json.name("nesting");
		json.value(t.getNestingKind().name());

		writeModifiers(t.getModifiers());
		writeTypeParams(t.getTypeParameters());

		json.name("superclass");
		TypeMirror sup = t.getSuperclass();
		if (sup.getKind() == javax.lang.model.type.TypeKind.NONE) {
			json.nullValue();
		} else {
			writeTypeMirror(sup, 0);
		}

		json.name("interfaces");
		json.beginArray();
		for (TypeMirror i : t.getInterfaces()) {
			writeTypeMirror(i, 0);
		}
		json.endArray();

		json.name("permits");
		json.beginArray();
		for (TypeMirror p : t.getPermittedSubclasses()) {
			writeTypeMirror(p, 0);
		}
		json.endArray();

		json.name("recordComponents");
		json.beginArray();
		for (RecordComponentElement rc : t.getRecordComponents()) {
			json.beginObject();
			json.name("name");
			json.value(rc.getSimpleName().toString());
			json.name("type");
			writeTypeMirror(rc.asType(), 0);
			json.name("accessor");
			json.value(rc.getAccessor() == null ? null : rc.getAccessor().getSimpleName().toString());
			writeAnnotations(rc.getAnnotationMirrors());
			writeDoc(rc);
			json.endObject();
		}
		json.endArray();

		writeAnnotations(t.getAnnotationMirrors());
		json.name("deprecated");
		json.value(elements.isDeprecated(t));
		writeDoc(t);
		writePosition(t);

		List<VariableElement> fields = new ArrayList<>();
		List<VariableElement> enumConstants = new ArrayList<>();
		List<ExecutableElement> constructors = new ArrayList<>();
		List<ExecutableElement> methods = new ArrayList<>();
		List<TypeElement> nested = new ArrayList<>();
		for (Element e : t.getEnclosedElements()) {
			ElementKind kind = e.getKind();
			if (kind == ElementKind.FIELD) {
				fields.add((VariableElement) e);
			} else if (kind == ElementKind.ENUM_CONSTANT) {
				enumConstants.add((VariableElement) e);
			} else if (kind == ElementKind.CONSTRUCTOR) {
				constructors.add((ExecutableElement) e);
			} else if (kind == ElementKind.METHOD) {
				methods.add((ExecutableElement) e);
			} else if (e instanceof TypeElement nt) {
				nested.add(nt);
			}
		}

		json.name("fields");
		json.beginArray();
		for (VariableElement f : fields) {
			writeField(f);
		}
		json.endArray();

		json.name("enumConstants");
		json.beginArray();
		for (VariableElement c : enumConstants) {
			writeEnumConstant(c);
		}
		json.endArray();

		json.name("constructors");
		json.beginArray();
		for (ExecutableElement c : constructors) {
			writeExecutable(c, true);
		}
		json.endArray();

		json.name("methods");
		json.beginArray();
		for (ExecutableElement m : methods) {
			writeExecutable(m, false);
		}
		json.endArray();

		json.name("nested");
		json.beginArray();
		for (TypeElement n : nested) {
			json.value(n.getQualifiedName().toString());
		}
		json.endArray();

		json.endObject();
	}

	// ------------------------------------------------------------------
	// Members
	// ------------------------------------------------------------------

	private void writeField(VariableElement f) {
		json.beginObject();
		json.name("name");
		json.value(f.getSimpleName().toString());
		json.name("type");
		writeTypeMirror(f.asType(), 0);
		writeModifiers(f.getModifiers());
		json.name("constant");
		Object constant = f.getConstantValue();
		if (constant == null) {
			json.nullValue();
		} else {
			writeConstant(constant);
		}
		writeAnnotations(f.getAnnotationMirrors());
		json.name("deprecated");
		json.value(elements.isDeprecated(f));
		json.name("origin");
		json.value(elements.getOrigin(f).name());
		writeDoc(f);
		writePosition(f);
		json.endObject();
	}

	private void writeEnumConstant(VariableElement c) {
		json.beginObject();
		json.name("name");
		json.value(c.getSimpleName().toString());
		writeAnnotations(c.getAnnotationMirrors());
		json.name("deprecated");
		json.value(elements.isDeprecated(c));
		writeDoc(c);

		// Constructor arguments and constant class bodies only exist in
		// source, not the element model (javadoc's attributed trees drop the
		// enum-constant initializer). Emit the raw declaration source slice
		// verbatim; the Rust side parses out `(args)` and `{ body }`.
		json.name("source");
		String source = null;
		TreePath path = trees.getPath(c);
		if (path != null) {
			try {
				CompilationUnitTree cu = path.getCompilationUnit();
				SourcePositions positions = trees.getSourcePositions();
				long start = positions.getStartPosition(cu, path.getLeaf());
				if (start >= 0) {
					// End positions are not retained by javadoc's parser, so
					// emit a fixed window; Rust parses the balanced `(args)`
					// group and `{` body opener and ignores the tail.
					CharSequence content = cu.getSourceFile().getCharContent(true);
					int from = (int) Math.min(start, content.length());
					int to = (int) Math.min(start + 2048, content.length());
					source = content.subSequence(from, to).toString();
				}
			} catch (java.io.IOException ignored) {
				// Source unavailable; leave null.
			}
		}
		json.value(source);
		writePosition(c);
		json.endObject();
	}

	private void writeExecutable(ExecutableElement e, boolean constructor) {
		json.beginObject();
		json.name("name");
		json.value(e.getSimpleName().toString());
		writeModifiers(e.getModifiers());
		writeTypeParams(e.getTypeParameters());

		json.name("params");
		json.beginArray();
		for (VariableElement p : e.getParameters()) {
			json.beginObject();
			json.name("name");
			json.value(p.getSimpleName().toString());
			json.name("type");
			writeTypeMirror(p.asType(), 0);
			writeAnnotations(p.getAnnotationMirrors());
			json.endObject();
		}
		json.endArray();

		json.name("return");
		if (constructor) {
			json.nullValue();
		} else {
			writeTypeMirror(e.getReturnType(), 0);
		}

		json.name("thrown");
		json.beginArray();
		for (TypeMirror thrown : e.getThrownTypes()) {
			writeTypeMirror(thrown, 0);
		}
		json.endArray();

		json.name("varargs");
		json.value(e.isVarArgs());
		json.name("default");
		json.value(e.isDefault());

		json.name("receiver");
		TypeMirror receiver = e.getReceiverType();
		if (receiver == null || receiver.getKind() == javax.lang.model.type.TypeKind.NONE) {
			json.nullValue();
		} else {
			writeTypeMirror(receiver, 0);
		}

		// Annotation-interface element default (e.g. `int retries() default 3`).
		json.name("annotationDefault");
		AnnotationValue dv = e.getDefaultValue();
		if (dv == null) {
			json.nullValue();
		} else {
			writeAnnotationValue(dv, 0);
		}

		writeAnnotations(e.getAnnotationMirrors());
		json.name("deprecated");
		json.value(elements.isDeprecated(e));
		json.name("origin");
		json.value(elements.getOrigin(e).name());
		writeDoc(e);
		writePosition(e);
		json.endObject();
	}

	// ------------------------------------------------------------------
	// Shared fragments
	// ------------------------------------------------------------------

	private void writeModifiers(Set<Modifier> modifiers) {
		json.name("modifiers");
		json.beginArray();
		for (Modifier m : modifiers) {
			json.value(m.toString());
		}
		json.endArray();
	}

	private void writeTypeParams(List<? extends TypeParameterElement> params) {
		json.name("typeParams");
		json.beginArray();
		for (TypeParameterElement p : params) {
			json.beginObject();
			json.name("name");
			json.value(p.getSimpleName().toString());
			json.name("bounds");
			json.beginArray();
			for (TypeMirror b : p.getBounds()) {
				writeTypeMirror(b, 0);
			}
			json.endArray();
			writeAnnotations(p.getAnnotationMirrors());
			json.endObject();
		}
		json.endArray();
	}

	/**
	 * Recursive structural type mirror. Tagged by "kind":
	 * primitive | void | declared | array | typevar | wildcard | intersection
	 * | union | error | none | null | other.
	 */
	private void writeTypeMirror(TypeMirror t, int depth) {
		if (depth > 64) {
			// Type mirrors reached from uses are finite, but guard anyway.
			json.beginObject();
			json.name("kind");
			json.value("other");
			json.name("repr");
			json.value(t.toString());
			json.endObject();
			return;
		}
		json.beginObject();
		switch (t.getKind()) {
			case BOOLEAN, BYTE, SHORT, INT, LONG, CHAR, FLOAT, DOUBLE -> {
				json.name("kind");
				json.value("primitive");
				json.name("name");
				json.value(t.getKind().name().toLowerCase(Locale.ROOT));
				writeAnnotations(t.getAnnotationMirrors());
			}
			case VOID -> {
				json.name("kind");
				json.value("void");
			}
			case DECLARED -> {
				DeclaredType d = (DeclaredType) t;
				TypeElement el = (TypeElement) d.asElement();
				json.name("kind");
				json.value("declared");
				json.name("name");
				json.value(el.getQualifiedName().toString());
				json.name("args");
				json.beginArray();
				for (TypeMirror arg : d.getTypeArguments()) {
					writeTypeMirror(arg, depth + 1);
				}
				json.endArray();
				// A generic owner (`Outer<T>.Inner`) is only interesting when it
				// carries type arguments of its own.
				TypeMirror owner = d.getEnclosingType();
				json.name("owner");
				if (owner.getKind() == javax.lang.model.type.TypeKind.DECLARED
					&& !((DeclaredType) owner).getTypeArguments().isEmpty()) {
					writeTypeMirror(owner, depth + 1);
				} else {
					json.nullValue();
				}
				writeAnnotations(t.getAnnotationMirrors());
			}
			case ARRAY -> {
				json.name("kind");
				json.value("array");
				json.name("component");
				writeTypeMirror(((ArrayType) t).getComponentType(), depth + 1);
				writeAnnotations(t.getAnnotationMirrors());
			}
			case TYPEVAR -> {
				json.name("kind");
				json.value("typevar");
				json.name("name");
				json.value(((TypeVariable) t).asElement().getSimpleName().toString());
				writeAnnotations(t.getAnnotationMirrors());
			}
			case WILDCARD -> {
				WildcardType w = (WildcardType) t;
				json.name("kind");
				json.value("wildcard");
				json.name("extends");
				if (w.getExtendsBound() == null) {
					json.nullValue();
				} else {
					writeTypeMirror(w.getExtendsBound(), depth + 1);
				}
				json.name("super");
				if (w.getSuperBound() == null) {
					json.nullValue();
				} else {
					writeTypeMirror(w.getSuperBound(), depth + 1);
				}
			}
			case INTERSECTION -> {
				json.name("kind");
				json.value("intersection");
				json.name("bounds");
				json.beginArray();
				for (TypeMirror b : ((IntersectionType) t).getBounds()) {
					writeTypeMirror(b, depth + 1);
				}
				json.endArray();
			}
			case UNION -> {
				json.name("kind");
				json.value("union");
				json.name("alternatives");
				json.beginArray();
				for (TypeMirror a : ((UnionType) t).getAlternatives()) {
					writeTypeMirror(a, depth + 1);
				}
				json.endArray();
			}
			case ERROR -> {
				json.name("kind");
				json.value("error");
				json.name("name");
				json.value(t.toString());
			}
			case NONE -> {
				json.name("kind");
				json.value("none");
			}
			case NULL -> {
				json.name("kind");
				json.value("null");
			}
			default -> {
				json.name("kind");
				json.value("other");
				json.name("repr");
				json.value(t.toString());
			}
		}
		json.endObject();
	}

	private void writeAnnotations(List<? extends AnnotationMirror> annotations) {
		json.name("annotations");
		json.beginArray();
		for (AnnotationMirror a : annotations) {
			writeAnnotation(a, 0);
		}
		json.endArray();
	}

	private void writeAnnotation(AnnotationMirror a, int depth) {
		json.beginObject();
		json.name("type");
		Element el = a.getAnnotationType().asElement();
		if (el instanceof TypeElement te) {
			json.value(te.getQualifiedName().toString());
		} else {
			json.value(a.getAnnotationType().toString());
		}
		json.name("values");
		json.beginObject();
		// Explicit values only — defaults live on the annotation declaration.
		for (Map.Entry<? extends ExecutableElement, ? extends AnnotationValue> entry
			: a.getElementValues().entrySet()) {
			json.name(entry.getKey().getSimpleName().toString());
			writeAnnotationValue(entry.getValue(), depth + 1);
		}
		json.endObject();
		json.endObject();
	}

	private void writeAnnotationValue(AnnotationValue av, int depth) {
		Object v = av.getValue();
		if (depth > 32) {
			json.beginObject();
			json.name("kind");
			json.value("other");
			json.name("repr");
			json.value(String.valueOf(v));
			json.endObject();
			return;
		}
		if (v instanceof AnnotationMirror am) {
			json.beginObject();
			json.name("kind");
			json.value("annotation");
			json.name("value");
			writeAnnotation(am, depth + 1);
			json.endObject();
		} else if (v instanceof List<?> list) {
			json.beginObject();
			json.name("kind");
			json.value("array");
			json.name("values");
			json.beginArray();
			for (Object item : list) {
				writeAnnotationValue((AnnotationValue) item, depth + 1);
			}
			json.endArray();
			json.endObject();
		} else if (v instanceof TypeMirror tm) {
			json.beginObject();
			json.name("kind");
			json.value("type");
			json.name("value");
			writeTypeMirror(tm, depth + 1);
			json.endObject();
		} else if (v instanceof VariableElement ve) {
			// An enum constant reference.
			json.beginObject();
			json.name("kind");
			json.value("enum");
			json.name("type");
			Element owner = ve.getEnclosingElement();
			json.value(owner instanceof TypeElement te ? te.getQualifiedName().toString() : owner.toString());
			json.name("name");
			json.value(ve.getSimpleName().toString());
			json.endObject();
		} else {
			writeConstant(v);
		}
	}

	/** Write a compile-time constant (primitives and String only). */
	private void writeConstant(Object c) {
		json.beginObject();
		json.name("kind");
		if (c instanceof String s) {
			json.value("string");
			json.name("value");
			json.value(s);
		} else if (c instanceof Boolean b) {
			json.value("boolean");
			json.name("value");
			json.value(b.booleanValue());
		} else if (c instanceof Character ch) {
			json.value("char");
			json.name("value");
			json.value(String.valueOf(ch.charValue()));
		} else if (c instanceof Float || c instanceof Double) {
			double d = ((Number) c).doubleValue();
			if (Double.isFinite(d)) {
				json.value("double");
				json.name("value");
				json.value(d);
			} else {
				// JSON has no NaN/Infinity, and the Rust mirror types the
				// `double` kind as a bare f64 — degrade to `other` instead.
				json.value("other");
				json.name("repr");
				json.value(String.valueOf(c));
			}
		} else if (c instanceof Number n) {
			json.value("int");
			json.name("value");
			json.value(n.longValue());
		} else {
			json.value("other");
			json.name("repr");
			json.value(String.valueOf(c));
		}
		json.endObject();
	}

	/**
	 * Raw doc comment text, verbatim from {@link Elements#getDocComment}
	 * (leading asterisks / slashes already stripped by javac), plus the doc
	 * flavor ({@code TRADITIONAL} vs {@code END_OF_LINE} for JEP 467 Markdown
	 * comments) when the running JDK reports it.
	 */
	private void writeDoc(Element e) {
		json.name("doc");
		json.value(elements.getDocComment(e));
		json.name("docKind");
		String kind = null;
		if (docKindMethod != null) {
			try {
				Object k = docKindMethod.invoke(elements, e);
				kind = k == null ? null : k.toString();
			} catch (ReflectiveOperationException ignored) {
				// Leave the flavor unreported.
			}
		}
		json.value(kind);
	}

	private void writePosition(Element e) {
		json.name("position");
		TreePath path = trees.getPath(e);
		if (path == null) {
			json.nullValue();
			return;
		}
		CompilationUnitTree cu = path.getCompilationUnit();
		SourcePositions positions = trees.getSourcePositions();
		long start = positions.getStartPosition(cu, path.getLeaf());
		json.beginObject();
		json.name("file");
		json.value(cu.getSourceFile().getName());
		json.name("line");
		if (start >= 0) {
			json.value(cu.getLineMap().getLineNumber(start));
		} else {
			json.nullValue();
		}
		json.endObject();
	}
}

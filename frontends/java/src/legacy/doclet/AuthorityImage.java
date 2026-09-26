/**
 * Writes Java compiler facts as one immutable, checksummed binary image.
 * The writer keeps javac's typed facts and never reconstructs them from source text.
 * Rust validates the exact same fixed-width planes without building a JSON object tree.
 */
package nudox.oracle;

import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.IdentityHashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import javax.lang.model.element.Element;
import javax.lang.model.element.AnnotationMirror;
import javax.lang.model.element.ElementKind;
import javax.lang.model.element.ExecutableElement;
import javax.lang.model.element.Modifier;
import javax.lang.model.element.ModuleElement;
import javax.lang.model.element.PackageElement;
import javax.lang.model.element.TypeElement;
import javax.lang.model.element.VariableElement;
import javax.lang.model.element.RecordComponentElement;
import javax.lang.model.type.ArrayType;
import javax.lang.model.type.DeclaredType;
import javax.lang.model.type.IntersectionType;
import javax.lang.model.type.TypeMirror;
import javax.lang.model.type.TypeVariable;
import javax.lang.model.type.UnionType;
import javax.lang.model.type.WildcardType;
import javax.lang.model.util.Elements;

import com.sun.source.tree.AssignmentTree;
import com.sun.source.tree.ClassTree;
import com.sun.source.tree.CompilationUnitTree;
import com.sun.source.tree.CompoundAssignmentTree;
import com.sun.source.tree.ExpressionTree;
import com.sun.source.tree.IdentifierTree;
import com.sun.source.tree.ImportTree;
import com.sun.source.tree.MemberReferenceTree;
import com.sun.source.tree.MemberSelectTree;
import com.sun.source.tree.MethodInvocationTree;
import com.sun.source.tree.MethodTree;
import com.sun.source.tree.NewClassTree;
import com.sun.source.tree.PackageTree;
import com.sun.source.tree.Tree;
import com.sun.source.tree.UnaryTree;
import com.sun.source.tree.VariableTree;
import com.sun.source.doctree.DocCommentTree;
import com.sun.source.util.DocTrees;
import com.sun.source.util.JavacTask;
import com.sun.source.util.SourcePositions;
import com.sun.source.util.TreePath;
import com.sun.source.util.TreePathScanner;
import com.sun.source.util.Trees;

/** Emits the immutable Java authority image from one attributed compiler task. */
final class AuthorityImage {
	private static final byte[] MAGIC = { 'N', 'J', 'A', 'I' };
	private static final byte[] DOMAIN = "nudox.java.authority.image.sha256.v4\0".getBytes(StandardCharsets.US_ASCII);
	private static final byte[] BOUND_MAGIC = { 'N', 'J', 'A', 'B' };
	private static final byte[] BOUND_DOMAIN = "nudox.java.bound.authority.image.sha256.v1\0".getBytes(StandardCharsets.US_ASCII);
	private static final int VERSION = 4;
	private static final int SECTION_COUNT = 12;
	private static final int HEADER_BYTES = 48 + SECTION_COUNT * 16;
	private static final int BOUND_HEADER_BYTES = 80;
	private static final int ABSENT = -1;
	/** Closed resolved-use tag: a `new` expression targeting its constructor. */
	private static final byte TAG_CONSTRUCTOR_CALL = 1;
	/** Closed resolved-use tag: a read of a declared field. */
	private static final byte TAG_FIELD_READ = 2;
	/** Closed resolved-use tag: an assignment-target write of a declared field. */
	private static final byte TAG_FIELD_WRITE = 3;
	/** Closed resolved-use tag: a declared type named in type position. */
	private static final byte TAG_TYPE_USE = 4;
	/** Closed resolved-use tag: a read of an enum constant. */
	private static final byte TAG_ENUM_CONSTANT_USE = 5;
	private static final int PUBLIC = 1 << 0;
	private static final int PROTECTED = 1 << 1;
	private static final int PRIVATE = 1 << 2;
	private static final int ABSTRACT = 1 << 3;
	private static final int STATIC = 1 << 4;
	private static final int FINAL = 1 << 5;
	private static final int TRANSIENT = 1 << 6;
	private static final int VOLATILE = 1 << 7;
	private static final int SYNCHRONIZED = 1 << 8;
	private static final int NATIVE = 1 << 9;
	private static final int STRICTFP = 1 << 10;
	private static final int DEFAULT = 1 << 11;
	private static final int SEALED = 1 << 12;
	private static final int NON_SEALED = 1 << 13;

private final Trees trees;
	private final DocTrees docs;
	private final Elements elements;
	private final AtomTable atoms = new AtomTable();
	private final TypePool types = new TypePool();
	private final SymbolPool symbols = new SymbolPool();
	private final List<DeclarationRow> declarations = new ArrayList<>();
	// Source extents aligned one-for-one with `declarations` (UTF-16 units).
	private final List<SpanRow> spans = new ArrayList<>();
	// Resolved non-invocation uses, emitted per scanned unit in tree order.
	private final List<UseRow> uses = new ArrayList<>();
	// Emitted declaration elements mapped to their declaration coordinate.
	private final Map<Element, Integer> declaredCoordinates = new IdentityHashMap<>();
	private final List<List<ExtensionEntry>> extensions = new ArrayList<>();
	private final List<ReferenceRow> references = new ArrayList<>();
	private final Map<String, Boolean> emittedPackages = new LinkedHashMap<>();
	private final Map<String, Boolean> emittedModules = new LinkedHashMap<>();

	private AuthorityImage(JavacTask task) {
		trees = Trees.instance(task);
		docs = DocTrees.instance(task);
		elements = task.getElements();
	}

	static void write(JavacTask task, Iterable<? extends CompilationUnitTree> units, int release, Path sourceBinding, Path output)
		throws IOException {
		AuthorityImage image = new AuthorityImage(task);
		image.collect(units, sourceBinding);
		image.write(release, sourceBinding, output);
	}

	private void collect(Iterable<? extends CompilationUnitTree> units, Path sourceBinding) {
		// The binding unit is scanned first so package rows (deduplicated by
		// name) are claimed by the unit whose extents the image can carry.
		List<CompilationUnitTree> bindingUnits = new ArrayList<>();
		List<CompilationUnitTree> otherUnits = new ArrayList<>();
		for (CompilationUnitTree unit : units) {
			(isBindingUnit(unit, sourceBinding) ? bindingUnits : otherUnits).add(unit);
		}
		for (CompilationUnitTree unit : bindingUnits) scanUnit(unit, true);
		for (CompilationUnitTree unit : otherUnits) scanUnit(unit, false);
	}

	private void scanUnit(CompilationUnitTree unit, boolean binding) {
		new DeclarationScanner(unit, binding).scan(unit, null);
		new CallScanner(unit).scan(unit, null);
		new UseScanner(unit).scan(unit, null);
	}

	// True when the unit's source file is the exact bound compile source.
	// Declaration spans are recorded only for the binding unit, so every
	// emitted extent lives in the source bytes the image is bound to.
	private static boolean isBindingUnit(CompilationUnitTree unit, Path sourceBinding) {
		if (sourceBinding == null) return false;
		try {
			return java.nio.file.Paths.get(unit.getSourceFile().toUri()).toAbsolutePath().normalize()
				.equals(sourceBinding.toAbsolutePath().normalize());
		} catch (RuntimeException failure) {
			return false;
		}
	}

	// Raw UTF-16 start of one member-reference name token inside javac's
	// resolved extent, or -1 when the suffix cannot be proven. Targets are
	// never chosen here; a failed trim emits no row.
	private long memberReferenceNameStart(
		CompilationUnitTree unit,
		CharSequence source,
		MemberReferenceTree reference,
		String decodedName
	) {
		if (source == null || decodedName == null || decodedName.isEmpty()) return -1;
		long referenceEnd = trees.getSourcePositions().getEndPosition(unit, reference);
		ExpressionTree qualifier = reference.getQualifierExpression();
		if (qualifier == null || referenceEnd < 0 || referenceEnd > Integer.MAX_VALUE) return -1;
		long qualifierEnd = trees.getSourcePositions().getEndPosition(unit, qualifier);
		if (qualifierEnd < 0) return -1;
		return memberReferenceNameStart(source, (int) qualifierEnd, (int) referenceEnd, decodedName);
	}

	private static long memberReferenceNameStart(
		CharSequence source,
		int qualifierEnd,
		int referenceEnd,
		String decodedName
	) {
		if (qualifierEnd < 0 || referenceEnd < qualifierEnd || referenceEnd > source.length()) return -1;
		int[] index = { qualifierEnd };
		if (!consumeMemberReferenceColons(source, referenceEnd, index)) return -1;
		skipMemberReferenceTrivia(source, referenceEnd, index);
		if (index[0] < referenceEnd) {
			int[] probe = { index[0] };
			if (readCodePoint(source, referenceEnd, probe) == '<') {
				if (!skipMemberReferenceTypeArguments(source, referenceEnd, index)) return -1;
				skipMemberReferenceTrivia(source, referenceEnd, index);
			}
		}
		int nameStart = index[0];
		int[] verify = { nameStart };
		if (!matchesDecodedName(source, referenceEnd, verify, decodedName) || verify[0] != referenceEnd) return -1;
		return nameStart;
	}

	private static boolean consumeMemberReferenceColons(CharSequence source, int end, int[] index) {
		for (int colons = 0; colons < 2; colons++) {
			if (readCodePoint(source, end, index) != ':') return false;
		}
		return true;
	}

	private static void skipMemberReferenceTrivia(CharSequence source, int end, int[] index) {
		while (index[0] < end) {
			char raw = source.charAt(index[0]);
			if (raw == ' ' || raw == '\t' || raw == '\f' || raw == '\r' || raw == '\n') {
				index[0]++;
				continue;
			}
			if (raw == '/' && index[0] + 1 < end) {
				if (source.charAt(index[0] + 1) == '/') {
					index[0] += 2;
					while (index[0] < end && source.charAt(index[0]) != '\n' && source.charAt(index[0]) != '\r') index[0]++;
					continue;
				}
				if (source.charAt(index[0] + 1) == '*') {
					index[0] += 2;
					while (index[0] + 1 < end) {
						if (source.charAt(index[0]) == '*' && source.charAt(index[0] + 1) == '/') {
							index[0] += 2;
							break;
						}
						index[0]++;
					}
					continue;
				}
			}
			return;
		}
	}

	private static boolean skipMemberReferenceTypeArguments(CharSequence source, int end, int[] index) {
		if (index[0] >= end || source.charAt(index[0]) != '<') return false;
		int depth = 1;
		index[0]++;
		while (index[0] < end && depth > 0) {
			skipMemberReferenceTrivia(source, end, index);
			if (index[0] >= end) return false;
			char raw = source.charAt(index[0]);
			if (raw == '"') {
				if (!skipMemberReferenceStringLiteral(source, end, index)) return false;
				continue;
			}
			if (raw == '\'') {
				if (!skipMemberReferenceCharLiteral(source, end, index)) return false;
				continue;
			}
			int[] probe = { index[0] };
			int codePoint = readCodePoint(source, end, probe);
			if (codePoint < 0) return false;
			index[0] = probe[0];
			if (codePoint == '<') depth++;
			else if (codePoint == '>') depth--;
		}
		return depth == 0;
	}

	private static boolean skipMemberReferenceStringLiteral(CharSequence source, int end, int[] index) {
		if (index[0] >= end || source.charAt(index[0]) != '"') return false;
		index[0]++;
		while (index[0] < end) {
			char raw = source.charAt(index[0]);
			if (raw == '"') {
				index[0]++;
				return true;
			}
			if (raw == '\\') {
				if (!skipMemberReferenceEscape(source, end, index)) return false;
				continue;
			}
			index[0]++;
		}
		return false;
	}

	private static boolean skipMemberReferenceCharLiteral(CharSequence source, int end, int[] index) {
		if (index[0] >= end || source.charAt(index[0]) != '\'') return false;
		index[0]++;
		while (index[0] < end) {
			char raw = source.charAt(index[0]);
			if (raw == '\'') {
				index[0]++;
				return true;
			}
			if (raw == '\\') {
				if (!skipMemberReferenceEscape(source, end, index)) return false;
				continue;
			}
			index[0]++;
		}
		return false;
	}

	private static boolean skipMemberReferenceEscape(CharSequence source, int end, int[] index) {
		if (index[0] >= end || source.charAt(index[0]) != '\\') return false;
		if (index[0] + 1 < end && source.charAt(index[0] + 1) == 'u') {
			int start = index[0] + 2;
			while (start < end && source.charAt(start) == 'u') start++;
			if (start + 4 > end) return false;
			for (int offset = 0; offset < 4; offset++) {
				if (hexDigit(source.charAt(start + offset)) < 0) return false;
			}
			index[0] = start + 4;
			return true;
		}
		if (index[0] + 1 >= end) return false;
		index[0] += 2;
		return true;
	}

	private static boolean matchesDecodedName(CharSequence source, int end, int[] index, String decodedName) {
		for (int offset = 0; offset < decodedName.length(); ) {
			int expected = decodedName.codePointAt(offset);
			if (readCodePoint(source, end, index) != expected) return false;
			offset += Character.charCount(expected);
		}
		return true;
	}

	private static int readCodePoint(CharSequence source, int end, int[] index) {
		if (index[0] >= end) return -1;
		char raw = source.charAt(index[0]);
		if (raw == '\\' && index[0] + 1 < end && source.charAt(index[0] + 1) == 'u') {
			int start = index[0] + 2;
			while (start < end && source.charAt(start) == 'u') start++;
			if (start + 4 > end) return -1;
			int codePoint = 0;
			for (int offset = 0; offset < 4; offset++) {
				int digit = hexDigit(source.charAt(start + offset));
				if (digit < 0) return -1;
				codePoint = (codePoint << 4) | digit;
			}
			index[0] = start + 4;
			return codePoint;
		}
		if (Character.isHighSurrogate(raw) && index[0] + 1 < end && Character.isLowSurrogate(source.charAt(index[0] + 1))) {
			int codePoint = Character.toCodePoint(raw, source.charAt(index[0] + 1));
			index[0] += 2;
			return codePoint;
		}
		index[0]++;
		return raw;
	}

	private static int hexDigit(char value) {
		if (value >= '0' && value <= '9') return value - '0';
		if (value >= 'a' && value <= 'f') return value - 'a' + 10;
		if (value >= 'A' && value <= 'F') return value - 'A' + 10;
		return -1;
	}

	private CharSequence compilationUnitCharacters(CompilationUnitTree unit) {
		try {
			return unit.getSourceFile().getCharContent(true);
		} catch (IOException failure) {
			return null;
		}
	}

	private final class DeclarationScanner extends TreePathScanner<Void, Void> {
		private final CompilationUnitTree unit;
		private final SourcePositions positions;
		private final boolean binding;
		DeclarationScanner(CompilationUnitTree unit, boolean binding) {
			this.unit = unit;
			this.binding = binding;
			positions = trees.getSourcePositions();
		}
		@Override
		public Void visitPackage(PackageTree node, Void unused) {
			Element element = trees.getElement(getCurrentPath());
			if (element instanceof PackageElement pkg) emitPackage(pkg, getCurrentPath(), unit, binding);
			return super.visitPackage(node, unused);
		}
		@Override
		public Void visitClass(ClassTree node, Void unused) {
			Element element = trees.getElement(getCurrentPath());
			// Anonymous classes have no qualified name and are
			// method-body implementation artifacts: they carry no
			// declaration rows, and their bodies' invocations
			// attribute to the enclosing declared executable.
			if (element instanceof TypeElement type && !type.getSimpleName().isEmpty()) {
				emitType(type, getCurrentPath(), unit, binding);
			}
			return super.visitClass(node, unused);
		}
	}

	// One declaration's source extent in UTF-16 units, or both-absent when
	// the unit carries no positions for the tree or is not the bound source.
	private SpanRow spanOf(CompilationUnitTree unit, SourcePositions positions, Tree tree) {
		long start = positions.getStartPosition(unit, tree);
		long end = positions.getEndPosition(unit, tree);
		if (start < 0 || end < start || end > Integer.MAX_VALUE) return new SpanRow(ABSENT, ABSENT);
		return new SpanRow((int) start, (int) end);
	}

	private void emitType(TypeElement type, TreePath typePath, CompilationUnitTree unit, boolean binding) {
		emitPackage(elements.getPackageOf(type), null, unit, false);
		emitModule(elements.getModuleOf(type));
		SourcePositions positions = trees.getSourcePositions();
		SpanRow typeSpan = binding ? spanOf(unit, positions, typePath.getLeaf()) : new SpanRow(ABSENT, ABSENT);
		int typeDeclaration = declarations.size();
		addDeclaration(type, declaration(
			declarationKind(type.getKind()),
			type.getQualifiedName().toString(),
			ownerName(type.getEnclosingElement()),
			documentation(type, typePath), type, types.intern(type.asType()), ABSENT
		), typeSpan);
		Map<Element, TreePath> memberPaths = memberPaths(typePath);
		for (Element member : type.getEnclosedElements()) {
			Documentation doc = documentation(member, memberPaths.get(member));
			TreePath memberPath = memberPaths.get(member);
			SpanRow memberSpan = binding && memberPath != null
				? spanOf(unit, positions, memberPath.getLeaf())
				: new SpanRow(ABSENT, ABSENT);
			switch (member.getKind()) {
				case FIELD, ENUM_CONSTANT -> addDeclaration(member, declaration(
					member.getKind() == ElementKind.FIELD ? 8 : 9,
					member.getSimpleName().toString(), type.getQualifiedName().toString(),
					doc, member, types.intern(member.asType()), ABSENT
				), memberSpan);
				case CONSTRUCTOR, METHOD -> {
					ExecutableElement executable = (ExecutableElement) member;
					int symbol = symbols.intern(executable);
					addDeclaration(executable, declaration(
						member.getKind() == ElementKind.CONSTRUCTOR ? 10 : 11,
						executable.getSimpleName().toString(), type.getQualifiedName().toString(),
						doc, executable,
						executable.getKind() == ElementKind.CONSTRUCTOR ? ABSENT : types.intern(executable.getReturnType()),
						symbol
					), memberSpan);
				}
				default -> { }
			}
		}
		if (type.getRecordComponents() != null) {
			for (RecordComponentElement component : type.getRecordComponents()) {
				for (int index = typeDeclaration + 1; index < declarations.size(); index++) {
					DeclarationRow row = declarations.get(index);
					if (row.kind == 8 && row.name == atoms.intern(component.getSimpleName().toString())) {
						extensions.get(typeDeclaration).add(new ExtensionEntry(3, index)); break;
					}
				}
			}
		}
	}

	private void emitPackage(PackageElement pkg, TreePath path, CompilationUnitTree unit, boolean binding) {
		if (pkg.isUnnamed()) return;
		String name = pkg.getQualifiedName().toString();
		if (emittedPackages.putIfAbsent(name, Boolean.TRUE) == null) {
			SpanRow span = binding && path != null
				? spanOf(unit, trees.getSourcePositions(), path.getLeaf())
				: new SpanRow(ABSENT, ABSENT);
			addDeclaration(pkg, declaration(2, name, null, documentation(pkg, path), pkg, ABSENT, ABSENT), span);
		}
	}

	private void emitModule(ModuleElement module) {
		if (module == null || module.isUnnamed()) return;
		String name = module.getQualifiedName().toString();
		if (emittedModules.putIfAbsent(name, Boolean.TRUE) == null) {
			addDeclaration(module, declaration(1, name, null, documentation(module, null), module, ABSENT, ABSENT), new SpanRow(ABSENT, ABSENT));
		}
	}

	// Appends one declaration row with its aligned source extent and records
	// the emitted element's declaration coordinate for use attribution.
	private void addDeclaration(Element element, DeclarationRow row, SpanRow span) {
		declaredCoordinates.put(element, declarations.size());
		declarations.add(row);
		spans.add(span);
	}

	private DeclarationRow declaration(int kind, String name, String owner, Documentation doc, Element element, int type, int symbol) {
		List<ExtensionEntry> facts = new ArrayList<>();
		for (AnnotationMirror annotation : element.getAnnotationMirrors()) facts.add(new ExtensionEntry(2, atoms.intern(annotation.toString())));
		if (element instanceof ExecutableElement executable) for (TypeMirror thrown : executable.getThrownTypes()) facts.add(new ExtensionEntry(1, types.intern(thrown)));
		extensions.add(facts);
		return new DeclarationRow(kind, origin(element), doc.flavor, modifierMask(element.getModifiers()),
			atoms.intern(name), atoms.optional(owner), atoms.optional(doc.text), type, symbol);
	}

	private Map<Element, TreePath> memberPaths(TreePath typePath) {
		Map<Element, TreePath> paths = new IdentityHashMap<>();
		for (var member : ((ClassTree) typePath.getLeaf()).getMembers()) {
			TreePath memberPath = new TreePath(typePath, member);
			Element element = trees.getElement(memberPath);
			if (element != null) paths.put(element, memberPath);
		}
		return paths;
	}

	private Documentation documentation(Element element, TreePath path) {
		DocCommentTree tree = path == null ? docs.getDocCommentTree(element) : docs.getDocCommentTree(path);
		if (tree == null) return new Documentation(0, null);
		return new Documentation(1, tree.toString());
	}

	private static String ownerName(Element element) {
		return element instanceof TypeElement type ? type.getQualifiedName().toString() : null;
	}

	private static int declarationKind(ElementKind kind) {
		return switch (kind) {
			case CLASS -> 3;
			case INTERFACE -> 4;
			case RECORD -> 5;
			case ENUM -> 6;
			case ANNOTATION_TYPE -> 7;
			default -> throw new IllegalArgumentException("unsupported Java declaration kind: " + kind);
		};
	}

	private int origin(Element element) {
		return switch (elements.getOrigin(element)) {
			case EXPLICIT -> 1;
			case MANDATED -> 2;
			case SYNTHETIC -> 3;
		};
	}

	private static int modifierMask(java.util.Set<Modifier> modifiers) {
		int mask = 0;
		for (Modifier modifier : modifiers) {
			mask |= switch (modifier) {
				case PUBLIC -> AuthorityImage.PUBLIC;
				case PROTECTED -> AuthorityImage.PROTECTED;
				case PRIVATE -> AuthorityImage.PRIVATE;
				case ABSTRACT -> AuthorityImage.ABSTRACT;
				case STATIC -> AuthorityImage.STATIC;
				case FINAL -> AuthorityImage.FINAL;
				case TRANSIENT -> AuthorityImage.TRANSIENT;
				case VOLATILE -> AuthorityImage.VOLATILE;
				case SYNCHRONIZED -> AuthorityImage.SYNCHRONIZED;
				case NATIVE -> AuthorityImage.NATIVE;
				case STRICTFP -> AuthorityImage.STRICTFP;
				case DEFAULT -> AuthorityImage.DEFAULT;
				case SEALED -> AuthorityImage.SEALED;
				case NON_SEALED -> AuthorityImage.NON_SEALED;
			};
		}
		return mask;
	}

	private final class CallScanner extends TreePathScanner<Void, Void> {
		private final CompilationUnitTree unit;
		private final SourcePositions positions;
		private final CharSequence source;
		CallScanner(CompilationUnitTree unit) {
			this.unit = unit;
			positions = trees.getSourcePositions();
			source = compilationUnitCharacters(unit);
		}
		@Override public Void visitMethodInvocation(MethodInvocationTree invocation, Void unused) {
			Element element = trees.getElement(getCurrentPath());
			ExecutableElement target = element instanceof ExecutableElement executable ? executable : null;
			ExecutableElement owner = enclosingExecutable(getCurrentPath());
			if (target != null && owner != null) {
				ExpressionTree select = invocation.getMethodSelect();
				long start = positions.getStartPosition(unit, select);
				long end = positions.getEndPosition(unit, select);
				if (select instanceof MemberSelectTree member && start >= 0 && end >= start) {
					long width = member.getIdentifier().length();
					if (width <= end - start) start = end - width;
				}
				if (start >= 0 && end >= start && end <= Integer.MAX_VALUE) {
					references.add(new ReferenceRow(symbols.intern(owner), symbols.intern(target),
						atoms.intern(unit.getSourceFile().getName()), (int) start, (int) end));
				}
			}
			return super.visitMethodInvocation(invocation, unused);
		}
		@Override public Void visitMemberReference(MemberReferenceTree reference, Void unused) {
			if (reference.getMode() != MemberReferenceTree.ReferenceMode.INVOKE) {
				return super.visitMemberReference(reference, unused);
			}
			Element element = trees.getElement(getCurrentPath());
			ExecutableElement target = element instanceof ExecutableElement executable ? executable : null;
			ExecutableElement owner = enclosingExecutable(getCurrentPath());
			if (target != null && owner != null) {
				long end = positions.getEndPosition(unit, reference);
				long start = memberReferenceNameStart(unit, source, reference, reference.getName().toString());
				if (start >= 0 && end >= start && end <= Integer.MAX_VALUE) {
					references.add(new ReferenceRow(symbols.intern(owner), symbols.intern(target),
						atoms.intern(unit.getSourceFile().getName()), (int) start, (int) end));
				}
			}
			return super.visitMemberReference(reference, unused);
		}
		private ExecutableElement enclosingExecutable(TreePath path) {
			for (TreePath current = path.getParentPath(); current != null; current = current.getParentPath()) {
				// Only a declared executable lexically owns an invocation: a
				// MethodInvocationTree node resolves to its callee, so honoring
				// bare ExecutableElements would attribute argument-nested calls
				// to a foreign callee.
				if (!(current.getLeaf() instanceof MethodTree)) continue;
				Element element = trees.getElement(current);
				if (!(element instanceof ExecutableElement executable)) continue;
				// Anonymous classes emit no declaration rows; their methods are
				// not owners. Walk past them to the enclosing declared executable.
				Element host = executable.getEnclosingElement();
				if (host instanceof TypeElement type && type.getSimpleName().isEmpty()) continue;
				return executable;
			}
			return null;
		}
	}

	// Records compiler-resolved non-invocation uses: `new` expressions,
	// field reads and assignment-target writes, declared types in type
	// position, and enum-constant reads. Every use resolves through
	// `Trees.getElement`, keeps its written name-token extent, and
	// attributes to the innermost declared executable or declared type
	// that lexically encloses it. Imports are skipped: they are scoped
	// names, not uses.
	private final class UseScanner extends TreePathScanner<Void, Void> {
		private final CompilationUnitTree unit;
		private final SourcePositions positions;
		private final CharSequence source;
		UseScanner(CompilationUnitTree unit) {
			this.unit = unit;
			positions = trees.getSourcePositions();
			source = compilationUnitCharacters(unit);
		}
		@Override public Void visitImport(ImportTree node, Void unused) {
			return null;
		}
		@Override public Void visitMemberReference(MemberReferenceTree reference, Void unused) {
			if (reference.getMode() == MemberReferenceTree.ReferenceMode.NEW) {
				Element element = trees.getElement(getCurrentPath());
				if (element instanceof ExecutableElement executable) {
					recordConstructorUse(getCurrentPath(), reference, executable);
				}
			}
			return super.visitMemberReference(reference, unused);
		}
		@Override public Void visitNewClass(NewClassTree node, Void unused) {
			Element element = trees.getElement(getCurrentPath());
			if (element instanceof ExecutableElement executable) {
				recordConstructorUse(getCurrentPath(), node.getIdentifier(), executable);
			}
			// The constructed type's name token is the constructor call's
			// extent, so scanning it again would double-record the token.
			scan(node.getEnclosingExpression(), unused);
			scan(node.getArguments(), unused);
			scan(node.getClassBody(), unused);
			return null;
		}
		@Override public Void visitMemberSelect(MemberSelectTree node, Void unused) {
			// `Outer.this` resolves to a type but names no type use.
			if (!node.getIdentifier().contentEquals("this")) {
				recordUse(getCurrentPath(), node, trees.getElement(getCurrentPath()), false);
			}
			return super.visitMemberSelect(node, unused);
		}
		@Override public Void visitIdentifier(IdentifierTree node, Void unused) {
			if (!node.getName().contentEquals("this") && !node.getName().contentEquals("super")) {
				recordUse(getCurrentPath(), node, trees.getElement(getCurrentPath()), false);
			}
			return super.visitIdentifier(node, unused);
		}
		@Override public Void visitAssignment(AssignmentTree node, Void unused) {
			writeScan(node.getVariable());
			scan(node.getExpression(), unused);
			return null;
		}
		@Override public Void visitCompoundAssignment(CompoundAssignmentTree node, Void unused) {
			writeScan(node.getVariable());
			scan(node.getExpression(), unused);
			return null;
		}
		@Override public Void visitUnary(UnaryTree node, Void unused) {
			switch (node.getKind()) {
				case POSTFIX_INCREMENT, POSTFIX_DECREMENT, PREFIX_INCREMENT, PREFIX_DECREMENT -> {
					writeScan(node.getExpression());
					return null;
				}
				default -> { return super.visitUnary(node, unused); }
			}
		}
// Records one assignment-target write when the target names a
// declared field; a qualified target still yields its receiver's
// reads, and any other target shape (array access, parenthesized
// form) keeps all of its inner reads.
		private void writeScan(Tree variable) {
			if (variable instanceof IdentifierTree) {
				recordUse(new TreePath(getCurrentPath(), variable), variable,
					trees.getElement(new TreePath(getCurrentPath(), variable)), true);
				return;
			}
			if (variable instanceof MemberSelectTree member) {
				recordUse(new TreePath(getCurrentPath(), variable), variable,
					trees.getElement(new TreePath(getCurrentPath(), variable)), true);
				scan(member.getExpression(), null);
				return;
			}
			scan(variable, null);
		}
// The closed image tag for one resolved element, or 0 when the
// element names no emitted declaration: local variables, parameters,
// bindings, packages, modules, type parameters, and executables in
// non-invocation position carry no image identity.
		private byte tagOf(Element element, boolean write) {
			if (element instanceof VariableElement variable) {
				if (variable.getKind() == ElementKind.ENUM_CONSTANT) return TAG_ENUM_CONSTANT_USE;
				if (variable.getKind() == ElementKind.FIELD) return write ? TAG_FIELD_WRITE : TAG_FIELD_READ;
				return 0;
			}
			if (element instanceof TypeElement) return TAG_TYPE_USE;
			return 0;
		}
		private void recordUse(TreePath usePath, Tree nameTree, Element element, boolean write) {
			byte tag = element == null ? 0 : tagOf(element, write);
			if (tag == 0) return;
			writeUseRow(usePath, nameTree, element, tag);
		}
		// A `new` expression resolves to its constructor executable; the row
		// keeps the invocation plane's symbol keying for its target and the
		// declaring type plus `<init>` spelling for its atoms.
		private void recordConstructorUse(TreePath usePath, Tree nameTree, Element element) {
			writeUseRow(usePath, nameTree, element, TAG_CONSTRUCTOR_CALL);
		}
		private void writeUseRow(TreePath usePath, Tree nameTree, Element element, byte tag) {
			Element owner = useOwner(usePath);
			if (owner == null) return;
			Integer ownerCoordinate = declaredCoordinates.get(owner);
			if (ownerCoordinate == null) return;
			long start = positions.getStartPosition(unit, nameTree);
			long end = positions.getEndPosition(unit, nameTree);
			if (nameTree instanceof MemberSelectTree member && start >= 0 && end >= start) {
				long width = member.getIdentifier().length();
				if (width <= end - start) start = end - width;
			} else if (nameTree instanceof MemberReferenceTree memberReference) {
				String decodedName = memberReference.getMode() == MemberReferenceTree.ReferenceMode.NEW
					? "new"
					: memberReference.getName().toString();
				start = memberReferenceNameStart(unit, source, memberReference, decodedName);
			}
			if (start < 0 || end < start || end > Integer.MAX_VALUE) return;
			String declaring;
			String name;
			if (element instanceof TypeElement type) {
				declaring = type.getQualifiedName().toString();
				name = null;
			} else {
				Element host = element.getEnclosingElement();
				declaring = host instanceof TypeElement type
					? type.getQualifiedName().toString()
					: host.toString();
				name = element.getSimpleName().toString();
			}
			uses.add(new UseRow(ownerCoordinate,
				tag == TAG_CONSTRUCTOR_CALL ? symbols.intern((ExecutableElement) element) : ABSENT,
				tag, atoms.intern(declaring), name == null ? ABSENT : atoms.intern(name),
				atoms.intern(unit.getSourceFile().getName()), (int) start, (int) end));
		}
// The innermost declared executable, or declared type when no
// executable encloses the use, walking past anonymous classes.
		private Element useOwner(TreePath path) {
			for (TreePath current = path.getParentPath(); current != null; current = current.getParentPath()) {
				if (current.getLeaf() instanceof MethodTree) {
					Element element = trees.getElement(current);
					if (element instanceof ExecutableElement executable) {
						Element host = executable.getEnclosingElement();
						if (host instanceof TypeElement type && type.getSimpleName().isEmpty()) continue;
						return executable;
					}
				}
				if (current.getLeaf() instanceof ClassTree) {
					Element element = trees.getElement(current);
					if (element instanceof TypeElement type && !type.getSimpleName().isEmpty()) return type;
				}
			}
			return null;
		}
	}

	private void write(int release, Path sourceBinding, Path output) throws IOException {
		byte[][] sections = { atoms.directory(), atoms.bytes(), types.rows(), types.edges(), symbols.rows(), symbols.parameters(), declarations(), references(), declarationExtensions(), extensionEntries(), uses(), spans() };
		int[] records = { 8, 1, 16, 4, 16, 8, 32, 20, 8, 8, 32, 8 };
		int[] counts = { atoms.count(), atoms.byteCount(), types.count(), types.edgeCount(), symbols.count(), symbols.parameterCount(), declarations.size(), references.size(), declarations.size(), extensionCount(), uses.size(), spans.size() };
		int bodyBytes = 0;
		for (byte[] section : sections) bodyBytes = Math.addExact(bodyBytes, section.length);
		byte[] header = header(release, bodyBytes, records, counts, sections);
		byte[] digest = digest(header, sections);
		System.arraycopy(digest, 0, header, 16, digest.length);
		int innerBytes = header.length;
		for (byte[] section : sections) innerBytes = Math.addExact(innerBytes, section.length);
		byte[] inner = new byte[innerBytes];
		int innerOffset = 0;
		System.arraycopy(header, 0, inner, innerOffset, header.length);
		innerOffset += header.length;
		for (byte[] section : sections) {
			System.arraycopy(section, 0, inner, innerOffset, section.length);
			innerOffset += section.length;
		}
		byte[] source = Files.readAllBytes(sourceBinding);
		byte[] bound = bound(source, inner);
		try (OutputStream stream = Files.newOutputStream(output)) {
			stream.write(bound);
		}
	}

	private static byte[] bound(byte[] source, byte[] inner) throws IOException {
		byte[] header = ByteBuffer.allocate(BOUND_HEADER_BYTES).order(ByteOrder.LITTLE_ENDIAN)
			.put(BOUND_MAGIC).putShort((short) 1).putShort((short) BOUND_HEADER_BYTES).putInt(inner.length).array();
		try {
			MessageDigest digest = MessageDigest.getInstance("SHA-256");
			System.arraycopy(digest.digest(source), 0, header, 12, 32);
			digest.update(BOUND_DOMAIN); digest.update(header, 0, 44); digest.update(header, 76, BOUND_HEADER_BYTES - 76); digest.update(inner);
			System.arraycopy(digest.digest(), 0, header, 44, 32);
			byte[] image = new byte[Math.addExact(header.length, inner.length)];
			System.arraycopy(header, 0, image, 0, header.length);
			System.arraycopy(inner, 0, image, header.length, inner.length);
			return image;
		} catch (NoSuchAlgorithmException error) {
			throw new IOException("the JDK does not provide SHA-256", error);
		}
	}

	private static byte[] header(int release, int bodyBytes, int[] records, int[] counts, byte[][] sections) {
		ByteBuffer header = ByteBuffer.allocate(HEADER_BYTES).order(ByteOrder.LITTLE_ENDIAN);
		header.put(MAGIC).putShort((short) VERSION).putShort((short) HEADER_BYTES).putShort((short) release).putShort((short) SECTION_COUNT).putInt(bodyBytes);
		header.position(48);
		int offset = HEADER_BYTES;
		for (int index = 0; index < SECTION_COUNT; index++) {
			header.putShort((short) (index + 1)).putShort((short) records[index]).putInt(counts[index]).putInt(offset).putInt(sections[index].length);
			offset = Math.addExact(offset, sections[index].length);
		}
		return header.array();
	}

	private static byte[] digest(byte[] header, byte[][] sections) throws IOException {
		try {
			MessageDigest digest = MessageDigest.getInstance("SHA-256");
			digest.update(DOMAIN); digest.update(header, 0, 16); digest.update(header, 48, HEADER_BYTES - 48);
			for (byte[] section : sections) digest.update(section);
			return digest.digest();
		} catch (NoSuchAlgorithmException error) {
			throw new IOException("the JDK does not provide SHA-256", error);
		}
	}

	private static final class AtomTable {
		private final Map<String, Integer> indices = new LinkedHashMap<>();
		private final ByteArrayOutputStream bytes = new ByteArrayOutputStream();
		private final List<Integer> offsets = new ArrayList<>();
		private final List<Integer> lengths = new ArrayList<>();
		int intern(String value) { return indices.computeIfAbsent(value, key -> { byte[] encoded = key.getBytes(StandardCharsets.UTF_8); int index = offsets.size(); offsets.add(bytes.size()); lengths.add(encoded.length); bytes.writeBytes(encoded); return index; }); }
		int optional(String value) { return value == null ? ABSENT : intern(value); }
		int count() { return offsets.size(); }
		int byteCount() { return bytes.size(); }
		byte[] bytes() { return bytes.toByteArray(); }
		byte[] directory() { ByteBuffer out = ByteBuffer.allocate(count() * 8).order(ByteOrder.LITTLE_ENDIAN); for (int index = 0; index < count(); index++) out.putInt(offsets.get(index)).putInt(lengths.get(index)); return out.array(); }
	}

	private final class TypePool {
		private final List<TypeRow> rows = new ArrayList<>();
		private final List<Integer> edges = new ArrayList<>();
		int intern(TypeMirror mirror) { return intern(mirror, 0); }
		private int intern(TypeMirror mirror, int depth) {
			if (depth > 64) throw new IllegalArgumentException("Java type nesting exceeds 64");
			int tag; int atom = ABSENT; int flags = 0;
			// Child type coordinates are interned into this row's own list
			// *before* the shared edge run is written, so a nested row's edges
			// can never interleave into this row's child range. The parent's
			// range is therefore exactly its ordered children.
			List<Integer> children = new ArrayList<>();
			switch (mirror.getKind()) {
				case BOOLEAN, BYTE, SHORT, INT, LONG, CHAR, FLOAT, DOUBLE -> { tag = 1; atom = atoms.intern(mirror.getKind().name().toLowerCase()); }
				case VOID -> tag = 2;
				case DECLARED -> { tag = 3; DeclaredType declared = (DeclaredType) mirror; atom = atoms.intern(((TypeElement) declared.asElement()).getQualifiedName().toString()); for (TypeMirror argument : declared.getTypeArguments()) children.add(intern(argument, depth + 1)); TypeMirror owner = declared.getEnclosingType(); if (owner.getKind() == javax.lang.model.type.TypeKind.DECLARED) { flags = 1; children.add(intern(owner, depth + 1)); } }
				case ARRAY -> { tag = 4; children.add(intern(((ArrayType) mirror).getComponentType(), depth + 1)); }
				case TYPEVAR -> {
					tag = 5;
					TypeVariable variable = (TypeVariable) mirror;
					String spelling = variable.asElement().getSimpleName().toString();
					// A type variable's upper bound is part of its identity:
					// two same-named variables with different bounds must stay
					// distinct. The bound is rendered into the spelling rather
					// than recursed as child coordinates, which would cycle for
					// F-bounded variables such as `<T extends Comparable<T>>`.
					TypeMirror upper = variable.getUpperBound();
					String bound = upper.toString();
					atom = atoms.intern("java.lang.Object".equals(bound) ? spelling : spelling + " extends " + bound);
				}
				case WILDCARD -> { tag = 6; WildcardType wildcard = (WildcardType) mirror; if (wildcard.getExtendsBound() != null) { flags |= 1; children.add(intern(wildcard.getExtendsBound(), depth + 1)); } if (wildcard.getSuperBound() != null) { flags |= 2; children.add(intern(wildcard.getSuperBound(), depth + 1)); } }
				case INTERSECTION -> { tag = 7; for (TypeMirror bound : ((IntersectionType) mirror).getBounds()) children.add(intern(bound, depth + 1)); }
				case UNION -> { tag = 8; for (TypeMirror option : ((UnionType) mirror).getAlternatives()) children.add(intern(option, depth + 1)); }
				case ERROR -> { tag = 9; atom = atoms.intern(mirror.toString()); }
				case NONE -> tag = 10;
				case NULL -> tag = 11;
				default -> throw new IllegalArgumentException("unsupported Java TypeKind: " + mirror.getKind());
			}
			int start = edges.size();
			for (int child : children) edges.add(child);
			int index = rows.size(); rows.add(new TypeRow(tag, flags, atom, start, children.size())); return index;
		}
		int count() { return rows.size(); } int edgeCount() { return edges.size(); }
		byte[] rows() { ByteBuffer out = ByteBuffer.allocate(count() * 16).order(ByteOrder.LITTLE_ENDIAN); for (TypeRow row : rows) out.put((byte) row.tag).put((byte) row.flags).putShort((short) row.children).putInt(row.atom).putInt(row.start).putInt(0); return out.array(); }
		byte[] edges() { ByteBuffer out = ByteBuffer.allocate(edgeCount() * 4).order(ByteOrder.LITTLE_ENDIAN); for (int edge : edges) out.putInt(edge); return out.array(); }
	}

	private final class SymbolPool {
		private final IdentityHashMap<ExecutableElement, Integer> indices = new IdentityHashMap<>();
		private final List<SymbolRow> rows = new ArrayList<>();
		private final List<Integer> parameterTypes = new ArrayList<>();
		private final List<Integer> parameterNames = new ArrayList<>();
		int intern(ExecutableElement element) {
			Integer known = indices.get(element); if (known != null) return known;
			int start = parameterTypes.size();
			for (VariableElement parameter : element.getParameters()) {
				parameterTypes.add(types.intern(parameter.asType()));
				// The declared parameter name travels beside its type coordinate
				// so the lane can name each carrier by the source name. A
				// compiler-synthesized parameter with no simple name stays
				// explicitly absent; the reader then falls back to the type
				// spelling rather than fabricating a name.
				String name = parameter.getSimpleName().toString();
				parameterNames.add(name.isEmpty() ? ABSENT : atoms.intern(name));
			}
			Element owner = element.getEnclosingElement(); String declaring = owner instanceof TypeElement type ? type.getQualifiedName().toString() : owner.toString(); int index = rows.size(); rows.add(new SymbolRow(atoms.intern(declaring), atoms.intern(element.getSimpleName().toString()), start, parameterTypes.size() - start)); indices.put(element, index); return index;
		}
		int count() { return rows.size(); } int parameterCount() { return parameterTypes.size(); }
		byte[] rows() { ByteBuffer out = ByteBuffer.allocate(count() * 16).order(ByteOrder.LITTLE_ENDIAN); for (SymbolRow row : rows) out.putInt(row.owner).putInt(row.name).putInt(row.start).putShort((short) row.count).putShort((short) 0); return out.array(); }
		byte[] parameters() { ByteBuffer out = ByteBuffer.allocate(parameterCount() * 8).order(ByteOrder.LITTLE_ENDIAN); for (int index = 0; index < parameterTypes.size(); index++) out.putInt(parameterTypes.get(index)).putInt(parameterNames.get(index)); return out.array(); }
	}

	private byte[] declarations() { ByteBuffer out = ByteBuffer.allocate(declarations.size() * 32).order(ByteOrder.LITTLE_ENDIAN); for (DeclarationRow row : declarations) out.put((byte) row.kind).put((byte) row.origin).put((byte) row.docFlavor).put((byte) 0).putInt(row.modifiers).putInt(row.name).putInt(row.owner).putInt(row.doc).putInt(ABSENT).putInt(row.type).putInt(row.symbol); return out.array(); }
	private byte[] references() { ByteBuffer out = ByteBuffer.allocate(references.size() * 20).order(ByteOrder.LITTLE_ENDIAN); for (ReferenceRow row : references) out.putInt(row.owner).putInt(row.target).putInt(row.file).putInt(row.start).putInt(row.end); return out.array(); }
	private byte[] uses() { ByteBuffer out = ByteBuffer.allocate(uses.size() * 32).order(ByteOrder.LITTLE_ENDIAN); for (UseRow row : uses) out.putInt(row.owner).putInt(row.target).put(row.tag).put(new byte[3]).putInt(row.declaring).putInt(row.name).putInt(row.file).putInt(row.start).putInt(row.end); return out.array(); }
	private byte[] spans() { ByteBuffer out = ByteBuffer.allocate(spans.size() * 8).order(ByteOrder.LITTLE_ENDIAN); for (SpanRow row : spans) out.putInt(row.start).putInt(row.end); return out.array(); }
	private byte[] declarationExtensions() { ByteBuffer out = ByteBuffer.allocate(declarations.size() * 8).order(ByteOrder.LITTLE_ENDIAN); int start = 0; for (List<ExtensionEntry> facts : extensions) { out.putInt(facts.isEmpty() ? 0 : start).putInt(facts.size()); start += facts.size(); } return out.array(); }
	private int extensionCount() { return extensions.stream().mapToInt(List::size).sum(); }
	private byte[] extensionEntries() { ByteBuffer out = ByteBuffer.allocate(extensionCount() * 8).order(ByteOrder.LITTLE_ENDIAN); for (List<ExtensionEntry> facts : extensions) for (ExtensionEntry entry : facts) out.put((byte) entry.tag).put(new byte[3]).putInt(entry.value); return out.array(); }
	private record Documentation(int flavor, String text) { }
	private record TypeRow(int tag, int flags, int atom, int start, int children) { }
	private record SymbolRow(int owner, int name, int start, int count) { }
	private record DeclarationRow(int kind, int origin, int docFlavor, int modifiers, int name, int owner, int doc, int type, int symbol) { }
	private record ReferenceRow(int owner, int target, int file, int start, int end) { }
	private record UseRow(int owner, int target, byte tag, int declaring, int name, int file, int start, int end) { }
	private record SpanRow(int start, int end) { }
	private record ExtensionEntry(int tag, int value) { }
}

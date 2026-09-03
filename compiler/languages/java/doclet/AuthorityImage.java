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

import com.sun.source.tree.ClassTree;
import com.sun.source.tree.CompilationUnitTree;
import com.sun.source.tree.ExpressionTree;
import com.sun.source.tree.MemberSelectTree;
import com.sun.source.tree.MethodInvocationTree;
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
	private static final byte[] DOMAIN = "nudox.java.authority.image.sha256.v2\0".getBytes(StandardCharsets.US_ASCII);
	private static final byte[] BOUND_MAGIC = { 'N', 'J', 'A', 'B' };
	private static final byte[] BOUND_DOMAIN = "nudox.java.bound.authority.image.sha256.v1\0".getBytes(StandardCharsets.US_ASCII);
	private static final int VERSION = 2;
	private static final int SECTION_COUNT = 10;
	private static final int HEADER_BYTES = 48 + SECTION_COUNT * 16;
	private static final int BOUND_HEADER_BYTES = 80;
	private static final int ABSENT = -1;
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
		image.collect(units);
		image.write(release, sourceBinding, output);
	}

	private void collect(Iterable<? extends CompilationUnitTree> units) {
		for (CompilationUnitTree unit : units) {
			new TreePathScanner<Void, Void>() {
				@Override
				public Void visitClass(ClassTree node, Void unused) {
					Element element = trees.getElement(getCurrentPath());
					if (element instanceof TypeElement type) emitType(type, getCurrentPath());
					return super.visitClass(node, unused);
				}
			}.scan(unit, null);
			new CallScanner(unit).scan(unit, null);
		}
	}

	private void emitType(TypeElement type, TreePath typePath) {
		emitPackage(elements.getPackageOf(type));
		emitModule(elements.getModuleOf(type));
		int typeDeclaration = declarations.size();
		declarations.add(declaration(
			declarationKind(type.getKind()),
			type.getQualifiedName().toString(),
			ownerName(type.getEnclosingElement()),
			documentation(type, typePath), type, types.intern(type.asType()), ABSENT
		));
		Map<Element, TreePath> memberPaths = memberPaths(typePath);
		for (Element member : type.getEnclosedElements()) {
			Documentation doc = documentation(member, memberPaths.get(member));
			switch (member.getKind()) {
				case FIELD, ENUM_CONSTANT -> declarations.add(declaration(
					member.getKind() == ElementKind.FIELD ? 8 : 9,
					member.getSimpleName().toString(), type.getQualifiedName().toString(),
					doc, member, types.intern(member.asType()), ABSENT
				));
				case CONSTRUCTOR, METHOD -> {
					ExecutableElement executable = (ExecutableElement) member;
					int symbol = symbols.intern(executable);
					declarations.add(declaration(
						member.getKind() == ElementKind.CONSTRUCTOR ? 10 : 11,
						executable.getSimpleName().toString(), type.getQualifiedName().toString(),
						doc, executable,
						executable.getKind() == ElementKind.CONSTRUCTOR ? ABSENT : types.intern(executable.getReturnType()),
						symbol
					));
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

	private void emitPackage(PackageElement pkg) {
		String name = pkg.getQualifiedName().toString();
		if (emittedPackages.putIfAbsent(name, Boolean.TRUE) == null) {
			declarations.add(declaration(2, name, null, documentation(pkg, null), pkg, ABSENT, ABSENT));
		}
	}

	private void emitModule(ModuleElement module) {
		if (module == null || module.isUnnamed()) return;
		String name = module.getQualifiedName().toString();
		if (emittedModules.putIfAbsent(name, Boolean.TRUE) == null) {
			declarations.add(declaration(1, name, null, documentation(module, null), module, ABSENT, ABSENT));
		}
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
		CallScanner(CompilationUnitTree unit) { this.unit = unit; positions = trees.getSourcePositions(); }
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
		private ExecutableElement enclosingExecutable(TreePath path) {
			for (TreePath current = path.getParentPath(); current != null; current = current.getParentPath()) {
				Element element = trees.getElement(current);
				if (element instanceof ExecutableElement executable) return executable;
			}
			return null;
		}
	}

	private void write(int release, Path sourceBinding, Path output) throws IOException {
		byte[][] sections = { atoms.directory(), atoms.bytes(), types.rows(), types.edges(), symbols.rows(), symbols.parameters(), declarations(), references(), declarationExtensions(), extensionEntries() };
		int[] records = { 8, 1, 16, 4, 16, 4, 32, 20, 8, 8 };
		int[] counts = { atoms.count(), atoms.byteCount(), types.count(), types.edgeCount(), symbols.count(), symbols.parameterCount(), declarations.size(), references.size(), declarations.size(), extensionCount() };
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
			int start = edges.size(); int tag; int atom = ABSENT; int flags = 0;
			switch (mirror.getKind()) {
				case BOOLEAN, BYTE, SHORT, INT, LONG, CHAR, FLOAT, DOUBLE -> { tag = 1; atom = atoms.intern(mirror.getKind().name().toLowerCase()); }
				case VOID -> tag = 2;
				case DECLARED -> { tag = 3; DeclaredType declared = (DeclaredType) mirror; atom = atoms.intern(((TypeElement) declared.asElement()).getQualifiedName().toString()); for (TypeMirror argument : declared.getTypeArguments()) edges.add(intern(argument, depth + 1)); TypeMirror owner = declared.getEnclosingType(); if (owner.getKind() == javax.lang.model.type.TypeKind.DECLARED) { flags = 1; edges.add(intern(owner, depth + 1)); } }
				case ARRAY -> { tag = 4; edges.add(intern(((ArrayType) mirror).getComponentType(), depth + 1)); }
				case TYPEVAR -> { tag = 5; atom = atoms.intern(((TypeVariable) mirror).asElement().getSimpleName().toString()); }
				case WILDCARD -> { tag = 6; WildcardType wildcard = (WildcardType) mirror; if (wildcard.getExtendsBound() != null) { flags |= 1; edges.add(intern(wildcard.getExtendsBound(), depth + 1)); } if (wildcard.getSuperBound() != null) { flags |= 2; edges.add(intern(wildcard.getSuperBound(), depth + 1)); } }
				case INTERSECTION -> { tag = 7; for (TypeMirror bound : ((IntersectionType) mirror).getBounds()) edges.add(intern(bound, depth + 1)); }
				case UNION -> { tag = 8; for (TypeMirror option : ((UnionType) mirror).getAlternatives()) edges.add(intern(option, depth + 1)); }
				case ERROR -> { tag = 9; atom = atoms.intern(mirror.toString()); }
				case NONE -> tag = 10;
				case NULL -> tag = 11;
				default -> throw new IllegalArgumentException("unsupported Java TypeKind: " + mirror.getKind());
			}
			int index = rows.size(); rows.add(new TypeRow(tag, flags, atom, start, edges.size() - start)); return index;
		}
		int count() { return rows.size(); } int edgeCount() { return edges.size(); }
		byte[] rows() { ByteBuffer out = ByteBuffer.allocate(count() * 16).order(ByteOrder.LITTLE_ENDIAN); for (TypeRow row : rows) out.put((byte) row.tag).put((byte) row.flags).putShort((short) row.children).putInt(row.atom).putInt(row.start).putInt(0); return out.array(); }
		byte[] edges() { ByteBuffer out = ByteBuffer.allocate(edgeCount() * 4).order(ByteOrder.LITTLE_ENDIAN); for (int edge : edges) out.putInt(edge); return out.array(); }
	}

	private final class SymbolPool {
		private final IdentityHashMap<ExecutableElement, Integer> indices = new IdentityHashMap<>();
		private final List<SymbolRow> rows = new ArrayList<>();
		private final List<Integer> parameters = new ArrayList<>();
		int intern(ExecutableElement element) { Integer known = indices.get(element); if (known != null) return known; int start = parameters.size(); for (VariableElement parameter : element.getParameters()) parameters.add(types.intern(parameter.asType())); Element owner = element.getEnclosingElement(); String declaring = owner instanceof TypeElement type ? type.getQualifiedName().toString() : owner.toString(); int index = rows.size(); rows.add(new SymbolRow(atoms.intern(declaring), atoms.intern(element.getSimpleName().toString()), start, parameters.size() - start)); indices.put(element, index); return index; }
		int count() { return rows.size(); } int parameterCount() { return parameters.size(); }
		byte[] rows() { ByteBuffer out = ByteBuffer.allocate(count() * 16).order(ByteOrder.LITTLE_ENDIAN); for (SymbolRow row : rows) out.putInt(row.owner).putInt(row.name).putInt(row.start).putShort((short) row.count).putShort((short) 0); return out.array(); }
		byte[] parameters() { ByteBuffer out = ByteBuffer.allocate(parameterCount() * 4).order(ByteOrder.LITTLE_ENDIAN); for (int parameter : parameters) out.putInt(parameter); return out.array(); }
	}

	private byte[] declarations() { ByteBuffer out = ByteBuffer.allocate(declarations.size() * 32).order(ByteOrder.LITTLE_ENDIAN); for (DeclarationRow row : declarations) out.put((byte) row.kind).put((byte) row.origin).put((byte) row.docFlavor).put((byte) 0).putInt(row.modifiers).putInt(row.name).putInt(row.owner).putInt(row.doc).putInt(ABSENT).putInt(row.type).putInt(row.symbol); return out.array(); }
	private byte[] references() { ByteBuffer out = ByteBuffer.allocate(references.size() * 20).order(ByteOrder.LITTLE_ENDIAN); for (ReferenceRow row : references) out.putInt(row.owner).putInt(row.target).putInt(row.file).putInt(row.start).putInt(row.end); return out.array(); }
	private byte[] declarationExtensions() { ByteBuffer out = ByteBuffer.allocate(declarations.size() * 8).order(ByteOrder.LITTLE_ENDIAN); int start = 0; for (List<ExtensionEntry> facts : extensions) { out.putInt(facts.isEmpty() ? 0 : start).putInt(facts.size()); start += facts.size(); } return out.array(); }
	private int extensionCount() { return extensions.stream().mapToInt(List::size).sum(); }
	private byte[] extensionEntries() { ByteBuffer out = ByteBuffer.allocate(extensionCount() * 8).order(ByteOrder.LITTLE_ENDIAN); for (List<ExtensionEntry> facts : extensions) for (ExtensionEntry entry : facts) out.put((byte) entry.tag).put(new byte[3]).putInt(entry.value); return out.array(); }
	private record Documentation(int flavor, String text) { }
	private record TypeRow(int tag, int flags, int atom, int start, int children) { }
	private record SymbolRow(int owner, int name, int start, int count) { }
	private record DeclarationRow(int kind, int origin, int docFlavor, int modifiers, int name, int owner, int doc, int type, int symbol) { }
	private record ReferenceRow(int owner, int target, int file, int start, int end) { }
	private record ExtensionEntry(int tag, int value) { }
}

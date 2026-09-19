import com.sun.source.tree.ClassTree;
import com.sun.source.tree.CompilationUnitTree;
import com.sun.source.tree.ExpressionTree;
import com.sun.source.tree.IdentifierTree;
import com.sun.source.tree.ImportTree;
import com.sun.source.tree.MemberSelectTree;
import com.sun.source.tree.MethodInvocationTree;
import com.sun.source.tree.MethodTree;
import com.sun.source.tree.ModuleTree;
import com.sun.source.tree.PackageTree;
import com.sun.source.tree.Tree;
import com.sun.source.tree.VariableTree;
import com.sun.source.util.DocTrees;
import com.sun.source.util.SourcePositions;
import com.sun.source.util.TreePath;
import com.sun.source.util.TreePathScanner;
import com.sun.source.util.Trees;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.URI;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.stream.Collectors;
import javax.lang.model.element.Element;
import javax.lang.model.element.ElementKind;
import javax.lang.model.element.ExecutableElement;
import javax.lang.model.element.ModuleElement;
import javax.lang.model.element.PackageElement;
import javax.lang.model.element.TypeElement;
import javax.lang.model.element.VariableElement;
import javax.lang.model.type.TypeMirror;
import javax.tools.Diagnostic;
import javax.tools.DiagnosticCollector;
import javax.tools.JavaCompiler;
import javax.tools.JavaFileObject;
import javax.tools.SimpleJavaFileObject;
import javax.tools.StandardJavaFileManager;
import javax.tools.ToolProvider;
import com.sun.source.doctree.DocCommentTree;
import com.sun.source.tree.DirectiveTree;
import com.sun.source.tree.RequiresTree;

/**
 * Executable javac authority for the backend BCQ/BCN protocol.
 *
 * <p>The request contains exact source bytes and the response contains only
 * facts obtained from javac's attributed trees. Source text is used for
 * materialisation and byte-coordinate conversion; it is never used to guess
 * a target for an occurrence. Unresolved symbols remain explicit records and
 * diagnostics are retained.</p>
 */
public final class BackendJavaSemantic {
    private static final String SCHEMA = "java-semantic-v1";
    private static final int MAX_RECORDS = 4096;
    private static final int MAX_VALUE_BYTES = 60 * 1024;
    private static final int MAX_INPUT_BYTES = 256 * 1024;

    private BackendJavaSemantic() {}

    public static void main(String[] ignored) throws Exception {
        Request request = Request.read(System.in);
        if (!"java".equals(request.language)) {
            throw new IOException("request language is not java");
        }

        Path root = Files.createTempDirectory("backend-java-authority-");
        try {
            List<Path> sources = materializeSources(request, root);
            List<Record> records = analyze(request, root, sources);
            records.sort(Comparator.comparingInt((Record row) -> row.kind)
                .thenComparing(row -> row.key));
            boolean complete = records.size() <= MAX_RECORDS;
            if (!complete) {
                records = new ArrayList<>(records.subList(0, MAX_RECORDS));
            }
            Envelope.write(System.out, request, records, complete);
        } finally {
            deleteTree(root);
        }
    }

    private static List<Path> materializeSources(Request request, Path root) throws IOException {
        List<Path> sources = new ArrayList<>();
        for (Map.Entry<String, byte[]> entry : request.inputs.entrySet()) {
            String name = entry.getKey().replace('\\', '/');
            if (!name.endsWith(".java")) {
                continue;
            }
            if (name.startsWith("/") || name.contains("\0")
                    || Arrays.stream(name.split("/", -1))
                        .anyMatch(part -> part.isEmpty() || ".".equals(part) || "..".equals(part))) {
                throw new IOException("source input escapes Java authority root: " + entry.getKey());
            }
            Path path = root.resolve(name).normalize();
            if (!path.startsWith(root) || entry.getValue().length > MAX_INPUT_BYTES) {
                throw new IOException("invalid Java source input: " + entry.getKey());
            }
            Files.createDirectories(path.getParent());
            Files.write(path, entry.getValue());
            sources.add(path);
        }
        if (sources.isEmpty()) {
            throw new IOException("BCQ request has no Java source input");
        }
        sources.sort(Comparator.comparing(Path::toString));
        return sources;
    }

    private static List<Record> analyze(Request request, Path root, List<Path> sourcePaths)
            throws IOException {
        JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        if (compiler == null) {
            throw new IOException("Java runtime has no system compiler");
        }

        String release = utf8(request.inputs.get("release"));
        if (release == null || !Set.of("8", "11", "17", "21", "25").contains(release)) {
            throw new IOException("unsupported Java release: " + release);
        }

        List<String> options = new ArrayList<>(List.of("-proc:none", "-XDkeepComments",
            "-Xlint:all", "--release", release));
        String classpath = classpath(request.inputs.get("classpath"));
        if (!classpath.isEmpty()) {
            options.addAll(List.of("-classpath", classpath));
        }

        DiagnosticCollector<JavaFileObject> diagnostics = new DiagnosticCollector<>();
        List<Record> records = new ArrayList<>();
        try (StandardJavaFileManager files = compiler.getStandardFileManager(
                diagnostics, Locale.ROOT, StandardCharsets.UTF_8)) {
            Iterable<? extends JavaFileObject> fileObjects =
                files.getJavaFileObjectsFromFiles(sourcePaths.stream().map(Path::toFile).toList());
            JavaCompiler.CompilationTask raw =
                compiler.getTask(null, files, diagnostics, options, null, fileObjects);
            if (!(raw instanceof com.sun.source.util.JavacTask task)) {
                throw new IOException("selected Java compiler is not JavacTask");
            }

            List<CompilationUnitTree> units = new ArrayList<>();
            try {
                task.parse().forEach(units::add);
                try {
                    task.analyze();
                } catch (RuntimeException ignored) {
                    // Javac diagnostics and partial attributed trees are still useful.
                }
            } catch (RuntimeException error) {
                records.add(new Record(4, "javac/parse",
                    value("error", "javac.parse", "0", "0", error.toString())));
            }

            Trees trees = Trees.instance(task);
            DocTrees docs = DocTrees.instance(task);
            Map<CompilationUnitTree, int[]> offsets = new HashMap<>();
            for (CompilationUnitTree unit : units) {
                offsets.put(unit, byteOffsets(unit));
                new Collector(trees, docs, unit, offsets.get(unit), records).scan(unit, null);
            }

            for (Diagnostic<? extends JavaFileObject> diagnostic : diagnostics.getDiagnostics()) {
                CompilationUnitTree unit = findUnit(units, diagnostic.getSource());
                int[] map = unit == null ? new int[] { 0 } : offsets.get(unit);
                long rawStart = diagnostic.getStartPosition();
                long rawEnd = diagnostic.getEndPosition();
                int start = toByte(map, rawStart);
                int end = toByte(map, rawEnd < rawStart ? rawStart : rawEnd);
                String severity = diagnosticSeverity(diagnostic.getKind());
                String code = clean(diagnostic.getCode() == null ? "javac" : diagnostic.getCode());
                add(records, 4, "diagnostic/" + code + "@" + start + ":" + end,
                    value(severity, code, Integer.toString(start), Integer.toString(end),
                        diagnostic.getMessage(Locale.ROOT)));
            }
        }

        // A complete authority claim closes optional dependency discovery.
        add(records, 5, "package:java.base", value("package", "java.base", "present"));
        for (String entry : classpath.split(java.util.regex.Pattern.quote(
                java.io.File.pathSeparator), -1)) {
            if (entry.isEmpty()) {
                continue;
            }
            Path jar = Path.of(entry);
            String name = jar.getFileName() == null ? entry : jar.getFileName().toString();
            String packageName = "classpath/" + clean(name);
            add(records, 5, "package:" + packageName,
                value("package", packageName, "present"));
        }
        add(records, 6, "package:classpath/optional",
            value("package", "classpath/optional", "absent"));
        return unique(records);
    }

    private static final class Collector extends TreePathScanner<Void, Void> {
        private final Trees trees;
        private final DocTrees docs;
        private final CompilationUnitTree unit;
        private final SourcePositions positions;
        private final int[] offsets;
        private final List<Record> records;
        private final Set<String> declarations = new HashSet<>();
        private final Set<String> references = new HashSet<>();

        Collector(Trees trees, DocTrees docs, CompilationUnitTree unit, int[] offsets,
                List<Record> records) {
            this.trees = trees;
            this.docs = docs;
            this.unit = unit;
            this.positions = trees.getSourcePositions();
            this.offsets = offsets;
            this.records = records;
        }

        @Override public Void visitPackage(PackageTree node, Void unused) {
            Element element = trees.getElement(getCurrentPath());
            if (element instanceof PackageElement packageElement) {
                declaration(packageElement, node);
                add(records, 5, "package:" + packageElement.getQualifiedName(),
                    value("package", packageElement.getQualifiedName().toString(), "present"));
            }
            return super.visitPackage(node, unused);
        }

        @Override public Void visitModule(ModuleTree node, Void unused) {
            Element element = trees.getElement(getCurrentPath());
            if (element instanceof ModuleElement module) {
                declaration(module, node);
                for (DirectiveTree directive : node.getDirectives()) {
                    if (directive instanceof RequiresTree requires) {
                        Element required = trees.getElement(new TreePath(getCurrentPath(), directive));
                        if (required instanceof ModuleElement requiredModule) {
                            String name = requiredModule.getQualifiedName().toString();
                            add(records, 5, "package:" + name,
                                value("package", name, "present"));
                        }
                    }
                }
            }
            return super.visitModule(node, unused);
        }

        @Override public Void visitClass(ClassTree node, Void unused) {
            Element element = trees.getElement(getCurrentPath());
            if (element instanceof TypeElement type && !type.getSimpleName().isEmpty()) {
                declaration(type, node);
            }
            return super.visitClass(node, unused);
        }

        @Override public Void visitMethod(MethodTree node, Void unused) {
            Element element = trees.getElement(getCurrentPath());
            if (element instanceof ExecutableElement executable) {
                declaration(executable, node);
            }
            return super.visitMethod(node, unused);
        }

        @Override public Void visitVariable(VariableTree node, Void unused) {
            Element element = trees.getElement(getCurrentPath());
            if (element instanceof VariableElement variable
                    && (variable.getKind() == ElementKind.FIELD
                        || variable.getKind() == ElementKind.ENUM_CONSTANT
                        || variable.getKind() == ElementKind.PARAMETER)) {
                declaration(variable, node);
            }
            return super.visitVariable(node, unused);
        }

        @Override public Void visitImport(ImportTree node, Void unused) {
            String imported = node.getQualifiedIdentifier().toString();
            String packageName = imported.endsWith(".*")
                ? imported.substring(0, imported.length() - 2)
                : imported.contains(".") ? imported.substring(0, imported.lastIndexOf('.')) : imported;
            add(records, 5, "package:" + clean(packageName),
                value("package", packageName, "present"));
            return super.visitImport(node, unused);
        }

        @Override public Void visitIdentifier(IdentifierTree node, Void unused) {
            reference(node, node);
            return super.visitIdentifier(node, unused);
        }

        @Override public Void visitMemberSelect(MemberSelectTree node, Void unused) {
            reference(node, node.getIdentifier());
            return super.visitMemberSelect(node, unused);
        }

        @Override public Void visitMethodInvocation(MethodInvocationTree node, Void unused) {
            Element target = trees.getElement(getCurrentPath());
            if (target != null) {
                reference(node, node.getMethodSelect());
            }
            return super.visitMethodInvocation(node, unused);
        }

        private void declaration(Element element, Tree node) {
            String key = symbolId(element);
            if (key.isEmpty() || !declarations.add(key)) {
                return;
            }
            Element enclosing = element.getEnclosingElement();
            String owner = enclosing == null ? "global" : symbolId(enclosing);
            String signature = clean(element.toString());
            if (signature.isEmpty()) {
                signature = clean(element.asType().toString());
            }
            DocCommentTree comment = docs.getDocCommentTree(getCurrentPath());
            String documentation = comment == null ? "" : clean(comment.toString());
            long rawStart = positions.getStartPosition(unit, getCurrentPath().getLeaf());
            long rawEnd = positions.getEndPosition(unit, getCurrentPath().getLeaf());
            String location = Path.of(unit.getSourceFile().toUri()).getFileName()
                + ":" + toByte(offsets, rawStart) + ":" + toByte(offsets, rawEnd);
            add(records, 1, key, value(element.getKind().name().toLowerCase(Locale.ROOT),
                owner, signature, documentation + "\u0001" + location));
            add(records, 2, key, value(key, clean(element.asType().toString())));
        }

        private void reference(Tree node, Tree spelling) {
            // The scanner's current path already points at the attributed
            // spelling (identifier/member select/invocation). Constructing a
            // second path below that leaf would create a path that is not in
            // the compilation unit and loses the symbol on javac versions
            // that validate TreePath ancestry.
            Element target = trees.getElement(getCurrentPath());
            Element owner = enclosingExecutable(getCurrentPath());
            if (owner == null || target == null || owner.equals(target)) {
                return;
            }
            long rawStart = positions.getStartPosition(unit, spelling);
            long rawEnd = positions.getEndPosition(unit, spelling);
            int start = toByte(offsets, rawStart);
            int end = toByte(offsets, rawEnd);
            if (rawStart < 0 || rawEnd < rawStart || end < start) {
                return;
            }
            String from = symbolId(owner);
            String to = symbolId(target);
            if (from.isEmpty() || to.isEmpty()) {
                return;
            }
            String key = from + "->" + to + "@" + start + ":" + end;
            if (references.add(key)) {
                add(records, 3, key, value(from, to, Integer.toString(start), Integer.toString(end)));
            }
        }

        private Element enclosingExecutable(TreePath path) {
            for (TreePath current = path.getParentPath(); current != null;
                    current = current.getParentPath()) {
                Tree leaf = current.getLeaf();
                if (!(leaf instanceof MethodTree)) {
                    continue;
                }
                Element element = trees.getElement(current);
                if (element instanceof ExecutableElement executable
                        && executable.getEnclosingElement() instanceof TypeElement) {
                    return executable;
                }
            }
            return null;
        }
    }

    private static String symbolId(Element element) {
        if (element == null) return "";
        return switch (element.getKind()) {
            case MODULE -> "M:" + ((ModuleElement) element).getQualifiedName();
            case PACKAGE -> "P:" + ((PackageElement) element).getQualifiedName();
            case CLASS, INTERFACE, ENUM, ANNOTATION_TYPE, RECORD ->
                "T:" + ((TypeElement) element).getQualifiedName();
            case METHOD, CONSTRUCTOR -> executableId((ExecutableElement) element);
            case FIELD, ENUM_CONSTANT -> "F:" + ownerQualified(element) + "#"
                + element.getSimpleName();
            case PARAMETER -> "P:" + ownerQualified(element) + "#" + element.getSimpleName();
            case TYPE_PARAMETER -> "TP:" + ownerQualified(element) + "#"
                + element.getSimpleName();
            default -> clean(element.toString());
        };
    }

    private static String executableId(ExecutableElement executable) {
        StringBuilder id = new StringBuilder("M:")
            .append(ownerQualified(executable)).append("#")
            .append(executable.getSimpleName()).append("(");
        for (int i = 0; i < executable.getParameters().size(); i++) {
            if (i > 0) id.append(",");
            id.append(executable.getParameters().get(i).asType());
        }
        return id.append(")").toString();
    }

    private static String ownerQualified(Element element) {
        Element owner = element.getEnclosingElement();
        if (owner instanceof TypeElement type) {
            return type.getQualifiedName().toString();
        }
        return owner == null ? "" : owner.toString();
    }

    private static List<Record> unique(List<Record> records) {
        Map<String, Record> unique = new TreeMap<>();
        for (Record row : records) {
            unique.putIfAbsent(row.kind + "\0" + row.key, row);
        }
        return new ArrayList<>(unique.values());
    }

    private static void add(List<Record> records, int kind, String key, String value) {
        records.add(new Record(kind, clean(key), clamp(value)));
    }

    private static String value(String... fields) {
        String[] clean = Arrays.stream(fields).map(BackendJavaSemantic::clean).toArray(String[]::new);
        return SCHEMA + "\0" + String.join("\0", clean);
    }

    private static String clean(String value) {
        return value == null ? "" : value.replace('\0', '\ufffd');
    }

    private static String utf8(byte[] bytes) {
        if (bytes == null) return null;
        try {
            return StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
                .decode(ByteBuffer.wrap(bytes)).toString();
        } catch (CharacterCodingException error) {
            return new String(bytes, StandardCharsets.UTF_8);
        }
    }

    private static String classpath(byte[] bytes) {
        String text = utf8(bytes);
        if (text == null || text.isEmpty()) return "";
        List<String> entries = new ArrayList<>();
        for (String candidate : text.split("[\\0\\r\\n" + java.util.regex.Pattern.quote(
                java.io.File.pathSeparator) + "]")) {
            if (!candidate.isEmpty() && Files.exists(Path.of(candidate))) {
                entries.add(candidate);
            }
        }
        return String.join(java.io.File.pathSeparator, entries);
    }

    private static String diagnosticSeverity(Diagnostic.Kind kind) {
        return switch (kind) {
            case ERROR -> "error";
            case WARNING -> "warning";
            case MANDATORY_WARNING -> "mandatory-warning";
            case NOTE -> "note";
            default -> "other";
        };
    }

    private static CompilationUnitTree findUnit(List<CompilationUnitTree> units, JavaFileObject source) {
        if (source == null) return null;
        for (CompilationUnitTree unit : units) {
            if (source.toUri().equals(unit.getSourceFile().toUri())) return unit;
        }
        return null;
    }

    private static int[] byteOffsets(CompilationUnitTree unit) {
        String text;
        try {
            text = unit.getSourceFile().getCharContent(true).toString();
        } catch (IOException error) {
            text = "";
        }
        int bom = text.startsWith("\ufeff") ? 3 : 0;
        if (bom != 0) text = text.substring(1);
        int[] offsets = new int[text.length() + 1];
        int bytes = bom;
        for (int i = 0; i < text.length();) {
            offsets[i] = bytes;
            int width = Character.isSupplementaryCodePoint(text.codePointAt(i)) ? 2 : 1;
            if (width == 2) offsets[i + 1] = bytes;
            // Javac may retain an unpaired surrogate from malformed input.
            // Match the UTF-8 replacement fallback without throwing while
            // constructing source-coordinate spans.
            bytes += width == 2 ? 4 : Character.isSurrogate(text.charAt(i)) ? 3
                : String.valueOf(text.charAt(i)).getBytes(StandardCharsets.UTF_8).length;
            i += width;
        }
        offsets[text.length()] = bytes;
        return offsets;
    }

    private static int toByte(int[] offsets, long position) {
        if (position < 0) return 0;
        int index = (int) Math.min(position, offsets.length - 1L);
        return offsets[index];
    }

    private static int toByte(int[] offsets, int position) {
        return toByte(offsets, (long) position);
    }

    private static String clamp(String text) {
        byte[] bytes = text.getBytes(StandardCharsets.UTF_8);
        if (bytes.length <= MAX_VALUE_BYTES) return text;
        return new String(bytes, 0, MAX_VALUE_BYTES - 3, StandardCharsets.UTF_8) + "...";
    }

    private record Record(int kind, String key, String value) {}

    private record Request(String language, byte[] session, byte[] manifest, byte[] authority,
            Map<String, byte[]> inputs) {
        static Request read(InputStream stream) throws IOException {
            byte[] body = stream.readAllBytes();
            Cursor cursor = new Cursor(body);
            if (!Arrays.equals(cursor.take(4), new byte[] {'B', 'C', 'Q', 0})
                    || cursor.u16() != 1) {
                throw new IOException("bad BCQ request");
            }
            int languageLength = cursor.u16();
            int count = cursor.u16();
            if (count > 256) {
                throw new IOException("too many BCQ input fields");
            }
            byte[] session = cursor.take(32);
            byte[] manifest = cursor.take(32);
            byte[] authority = cursor.take(32);
            String language = new String(cursor.take(languageLength), StandardCharsets.UTF_8);
            Map<String, byte[]> inputs = new TreeMap<>();
            for (int i = 0; i < count; i++) {
                int nameLength = cursor.u16();
                int valueLength = cursor.u32();
                String name = new String(cursor.take(nameLength), StandardCharsets.UTF_8);
                byte[] value = cursor.take(valueLength);
                if (inputs.putIfAbsent(name, value) != null) {
                    throw new IOException("duplicate BCQ input: " + name);
                }
            }
            if (!cursor.done()) throw new IOException("trailing BCQ bytes");
            return new Request(language, session, manifest, authority, inputs);
        }
    }

    private static final class Cursor {
        private final byte[] bytes;
        private int position;
        Cursor(byte[] bytes) { this.bytes = bytes; }
        byte[] take(int size) throws IOException {
            if (size < 0 || position > bytes.length - size) throw new IOException("truncated BCQ request");
            byte[] value = Arrays.copyOfRange(bytes, position, position + size);
            position += size;
            return value;
        }
        int u16() throws IOException {
            return ByteBuffer.wrap(take(2)).order(ByteOrder.BIG_ENDIAN).getShort() & 0xffff;
        }
        int u32() throws IOException {
            long value = ByteBuffer.wrap(take(4)).order(ByteOrder.BIG_ENDIAN).getInt() & 0xffffffffL;
            if (value > Integer.MAX_VALUE) throw new IOException("BCQ field exceeds Java bounds");
            return (int) value;
        }
        boolean done() { return position == bytes.length; }
    }

    private static final class Envelope {
        static void write(OutputStream output, Request request, List<Record> rows,
                boolean complete) throws IOException {
            List<Record> records = unique(rows);
            java.io.DataOutputStream data = new java.io.DataOutputStream(output);
            data.writeBytes("BCN\0");
            data.writeShort(1);
            data.writeByte(complete ? 0 : 1);
            data.writeByte(0);
            data.writeInt(records.size());
            data.write(request.session);
            data.write(request.manifest);
            data.write(request.authority);
            data.writeLong(0);
            byte[] language = request.language.getBytes(StandardCharsets.UTF_8);
            data.writeShort(language.length);
            data.write(language);
            for (Record row : records) {
                byte[] key = row.key.getBytes(StandardCharsets.UTF_8);
                byte[] value = row.value.getBytes(StandardCharsets.UTF_8);
                data.writeByte(row.kind);
                data.writeByte(0);
                data.writeInt(key.length);
                data.writeInt(value.length);
                data.write(key);
                data.write(value);
            }
            data.flush();
        }
    }

    private static void deleteTree(Path root) {
        try {
            Files.walk(root).sorted(Comparator.reverseOrder()).forEach(path -> {
                try { Files.deleteIfExists(path); } catch (IOException ignored) {}
            });
        } catch (IOException ignored) {}
    }

    private static final class SourceFile extends SimpleJavaFileObject {
        private final String source;
        SourceFile(String name, String source) {
            super(URI.create("string:///" + name), Kind.SOURCE);
            this.source = source;
        }
        @Override public CharSequence getCharContent(boolean ignoreEncodingErrors) {
            return source;
        }
    }
}

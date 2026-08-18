package nudox.oracle;

import com.sun.javadoc.AnnotationDesc;
import com.sun.javadoc.ClassDoc;
import com.sun.javadoc.ConstructorDoc;
import com.sun.javadoc.Doc;
import com.sun.javadoc.FieldDoc;
import com.sun.javadoc.MethodDoc;
import com.sun.javadoc.Parameter;
import com.sun.javadoc.RootDoc;
import com.sun.javadoc.Type;
import java.util.TreeSet;

/**
 * Java 8 javadoc adapter for source sets that cannot be attributed by modern
 * javac.  It deliberately emits the same format-1 envelope as Extractor.
 *
 * The old doclet API does not expose the javax.lang.model graph used by the
 * normal extractor, so this adapter emits the stable declaration surface and
 * uses TypeMirror::Other for type details it cannot represent structurally.
 * That is lossless for declaration identity and keeps the Rust lowering
 * contract unchanged.
 */
public final class LegacyExtractor {
    public static String getName() {
        return "NudoxLegacyExtractor";
    }

    public static int optionLength(String option) {
        return 0;
    }

    public static boolean start(RootDoc root) {
        StringBuilder out = new StringBuilder(1 << 20);
        out.append("{\"format\":1,\"javaVersion\":");
        string(out, System.getProperty("java.version"));
        ClassDoc[] classes = root.classes();
        TreeSet<String> packages = new TreeSet<String>();
        for (ClassDoc c : classes) {
            if (c.containingPackage() != null) packages.add(c.containingPackage().name());
        }
        out.append(",\"modules\":[],\"packages\":[");
        int packageIndex = 0;
        for (String name : packages) {
            if (packageIndex++ != 0) out.append(',');
            out.append("{\"name\":");
            string(out, name);
            out.append(",\"doc\":null,\"docKind\":null,\"annotations\":[],\"position\":null}");
        }
        out.append("],\"types\":[");
        for (int i = 0; i < classes.length; i++) {
            if (i != 0) out.append(',');
            type(out, classes[i]);
        }
        out.append("]}");
        System.out.println(out.toString());
        return true;
    }

    private static void type(StringBuilder out, ClassDoc c) {
        out.append('{');
        field(out, "qualifiedName", c.qualifiedName());
        out.append(',');
        field(out, "simpleName", c.name());
        out.append(',');
        field(out, "kind", c.isAnnotationType() ? "ANNOTATION_TYPE" :
            c.isEnum() ? "ENUM" : c.isInterface() ? "INTERFACE" : "CLASS");
        out.append(',');
        field(out, "package", c.containingPackage() == null ? "" : c.containingPackage().name());
        out.append(",\"module\":null,\"enclosing\":null,\"nesting\":\"")
            .append(c.containingClass() == null ? "TOP_LEVEL" : "MEMBER").append('"');
        modifiers(out, c.modifiers());
        out.append(",\"typeParams\":[],\"superclass\":");
        if (c.superclassType() == null) out.append("null"); else mirror(out, c.superclassType());
        out.append(",\"interfaces\":[] ,\"permits\":[],\"recordComponents\":[]");
        annotations(out, c.annotations());
        out.append(",\"deprecated\":").append(c.tags("deprecated").length != 0);
        doc(out, c);
        out.append(",\"position\":null,\"fields\":[");
        FieldDoc[] fields = c.fields(false);
        for (int i = 0; i < fields.length; i++) {
            if (i != 0) out.append(',');
            field(out, fields[i]);
        }
        out.append("],\"enumConstants\":[],\"constructors\":[");
        ConstructorDoc[] constructors = c.constructors(false);
        for (int i = 0; i < constructors.length; i++) {
            if (i != 0) out.append(',');
            executable(out, constructors[i], true);
        }
        out.append("],\"methods\":[");
        MethodDoc[] methods = c.methods(false);
        for (int i = 0; i < methods.length; i++) {
            if (i != 0) out.append(',');
            executable(out, methods[i], false);
        }
        out.append("],\"nested\":[");
        ClassDoc[] nested = c.innerClasses();
        for (int i = 0; i < nested.length; i++) {
            if (i != 0) out.append(',');
            string(out, nested[i].qualifiedName());
        }
        out.append("]}");
    }

    private static void field(StringBuilder out, FieldDoc f) {
        out.append('{');
        field(out, "name", f.name());
        out.append(",\"type\":"); mirror(out, f.type());
        modifiers(out, f.modifiers());
        out.append(",\"constant\":null");
        annotations(out, f.annotations());
        out.append(",\"deprecated\":").append(f.tags("deprecated").length != 0);
        out.append(",\"origin\":null");
        doc(out, f);
        out.append(",\"position\":null}");
    }

    private static void executable(StringBuilder out, Doc d, boolean constructor) {
        out.append('{');
        field(out, "name", constructor ? "<init>" : ((MethodDoc) d).name());
        modifiers(out, d instanceof ConstructorDoc ? ((ConstructorDoc) d).modifiers()
            : ((MethodDoc) d).modifiers());
        out.append(",\"typeParams\":[],\"params\":[");
        Parameter[] params = d instanceof ConstructorDoc ? ((ConstructorDoc) d).parameters()
            : ((MethodDoc) d).parameters();
        for (int i = 0; i < params.length; i++) {
            if (i != 0) out.append(',');
            out.append('{');
            field(out, "name", params[i].name());
            out.append(",\"type\":"); mirror(out, params[i].type());
            out.append(",\"annotations\":[]}");
        }
        out.append("],\"return\":");
        if (constructor) out.append("null"); else mirror(out, ((MethodDoc) d).returnType());
        out.append(",\"thrown\":[],\"varargs\":false,\"default\":false,\"receiver\":null,")
            .append("\"annotationDefault\":null");
        AnnotationDesc[] memberAnnotations = d instanceof ConstructorDoc
            ? ((ConstructorDoc) d).annotations() : ((MethodDoc) d).annotations();
        annotations(out, memberAnnotations);
        out.append(",\"deprecated\":").append(d.tags("deprecated").length != 0)
            .append(",\"origin\":null");
        doc(out, d);
        out.append(",\"position\":null}");
    }

    private static void mirror(StringBuilder out, Type t) {
        out.append("{\"kind\":\"other\",\"repr\":");
        string(out, t == null ? "" : t.toString());
        out.append('}');
    }

    private static void annotations(StringBuilder out, AnnotationDesc[] as) {
        out.append(",\"annotations\":[");
        if (as != null) for (int i = 0; i < as.length; i++) {
            if (i != 0) out.append(',');
            out.append("{\"type\":");
            string(out, as[i].annotationType().qualifiedName());
            out.append(",\"values\":{}}");
        }
        out.append(']');
    }

    private static void modifiers(StringBuilder out, String modifiers) {
        out.append(",\"modifiers\":[");
        if (modifiers != null && !modifiers.trim().isEmpty()) {
            String[] parts = modifiers.trim().split("\\s+");
            for (int i = 0; i < parts.length; i++) {
                if (i != 0) out.append(',');
                string(out, parts[i]);
            }
        }
        out.append(']');
    }

    private static void doc(StringBuilder out, Doc d) {
        out.append(",\"doc\":");
        string(out, d.commentText());
        out.append(",\"docKind\":null");
    }

    private static void field(StringBuilder out, String name, String value) {
        out.append('"').append(name).append("\":");
        string(out, value);
    }

    private static void string(StringBuilder out, String value) {
        if (value == null) {
            out.append("null");
            return;
        }
        out.append('"');
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            if (c == '"' || c == '\\') out.append('\\');
            if (c == '\n') out.append("\\n");
            else if (c == '\r') out.append("\\r");
            else if (c == '\t') out.append("\\t");
            else out.append(c);
        }
        out.append('"');
    }
}

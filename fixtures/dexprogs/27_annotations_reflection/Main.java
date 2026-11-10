import java.lang.annotation.Annotation;
import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * DEX annotation reflection: RUNTIME annotations on a class, a field, a method
 * and a parameter, with string/int/boolean/enum/class/array/nested elements and
 * defaults, plus the annotation contract (toString/equals/hashCode/
 * annotationType) and a generic superclass signature.
 */
@Retention(RetentionPolicy.RUNTIME)
@Target({ElementType.TYPE, ElementType.FIELD, ElementType.METHOD, ElementType.PARAMETER})
@interface Nested {
    String value();
    int n() default 3;
}

enum Color { RED, GREEN, BLUE }

@Retention(RetentionPolicy.RUNTIME)
@Target({ElementType.TYPE, ElementType.FIELD, ElementType.METHOD, ElementType.PARAMETER})
@interface Simple {
    String a() default "x";
    int b() default 1;
    int[] c() default {1, 2, 3};
}

@Retention(RetentionPolicy.RUNTIME)
@Target({ElementType.TYPE, ElementType.FIELD, ElementType.METHOD, ElementType.PARAMETER})
@interface Info {
    String name() default "def";
    int num() default 7;
    boolean flag() default true;
    Color color() default Color.GREEN;
    Class<?> type() default Object.class;
    int[] nums() default {1, 2, 3};
    String[] tags() default {};
    Nested nested() default @Nested("nn");
}

@Simple(a = "s", b = 5, c = {4, 5})
@Info(name = "cls", num = 42, color = Color.BLUE, type = String.class,
      nums = {5, 6}, tags = {"a", "b"}, nested = @Nested(value = "x", n = 9))
public class Main {
    @Info(name = "fld", flag = false)
    static String f = "x";

    @Info(num = 9, color = Color.RED, nums = {7})
    public static void m(@Info(name = "p", type = Integer.class) int a) {}

    static class ListHolder extends ArrayList<String> {}

    static void print(Info i) {
        System.out.println("toString=" + i);
        System.out.println("name=" + i.name());
        System.out.println("num=" + i.num());
        System.out.println("flag=" + i.flag());
        System.out.println("color=" + i.color());
        System.out.println("type=" + i.type().getName());
        System.out.println("nums=" + Arrays.toString(i.nums()));
        System.out.println("tags=" + Arrays.toString(i.tags()));
        System.out.println("nested=" + i.nested());
        System.out.println("nested.value=" + i.nested().value());
        System.out.println("nested.n=" + i.nested().n());
    }

    public static void main(String[] args) throws Exception {
        Class<?> c = Main.class;
        Info ci = c.getAnnotation(Info.class);
        System.out.println("class=" + ci);
        System.out.println("present=" + c.isAnnotationPresent(Info.class));
        System.out.println("annotationType=" + ci.annotationType().getName());
        System.out.println("equals=" + ci.equals(c.getAnnotation(Info.class)));
        // `hashCode` of an annotation with no enum/Class elements is
        // deterministic across runtimes (identity hashes are not).
        Simple sm = c.getAnnotation(Simple.class);
        System.out.println("hash=" + sm.hashCode());
        System.out.println("hashEq=" + sm.equals(c.getAnnotation(Simple.class)));
        print(ci);
        System.out.println("declaredCount=" + c.getDeclaredAnnotations().length);
        System.out.println("declared0=" + c.getDeclaredAnnotations()[0]);

        java.lang.reflect.Field fld = Main.class.getDeclaredField("f");
        Info fi = fld.getAnnotation(Info.class);
        System.out.println("field=" + fi);
        System.out.println("fieldPresent=" + fld.isAnnotationPresent(Info.class));
        System.out.println("field.name=" + fi.name());
        System.out.println("field.flag=" + fi.flag());
        System.out.println("field.color=" + fi.color());
        System.out.println("field.nums=" + Arrays.toString(fi.nums()));
        System.out.println("field.nested=" + fi.nested());

        java.lang.reflect.Method m = Main.class.getDeclaredMethod("m", int.class);
        Info mi = m.getAnnotation(Info.class);
        System.out.println("method=" + mi);
        System.out.println("method.name=" + mi.name());
        System.out.println("method.num=" + mi.num());
        System.out.println("method.color=" + mi.color());
        System.out.println("method.nums=" + Arrays.toString(mi.nums()));
        System.out.println("methodPresent=" + m.isAnnotationPresent(Info.class));
        System.out.println("methodDeclaredCount=" + m.getDeclaredAnnotations().length);

        Annotation[][] pa = m.getParameterAnnotations();
        System.out.println("param=" + pa[0][0]);
        System.out.println("paramPresent=" + pa[0][0].annotationType().getName());

        System.out.println("generic=" + ListHolder.class.getGenericSuperclass());
        System.out.println("genericIfaces=" + List.class.getGenericInterfaces().length);
    }
}

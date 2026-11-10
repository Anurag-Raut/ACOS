import java.util.regex.Matcher;
import java.util.regex.Pattern;

public class Main {
    static void p(String key, Object value) {
        System.out.println(key + "=" + value);
    }

    static String join(String[] a) {
        return "[" + String.join("|", a) + "]";
    }

    public static void main(String[] args) {
        p("split.colon", join("a:b:c".split(":")));
        p("split.double", join("a::b".split(":")));
        p("split.leading", join(":a:b".split(":")));
        p("split.trailing", join("a:b:".split(":")));
        p("split.limit2", join("a:b:c:d".split(":", 2)));
        p("split.limitNeg", join("a:b:c".split(":", -1)));
        p("split.regex", join("a1b22c".split("\\d+")));
        p("split.all", join("a,b,c".split("x")));

        Matcher m = Pattern.compile("(\\w+)=(\\w+)").matcher("k1=v1 k2=v2");
        StringBuilder sb = new StringBuilder();
        while (m.find()) {
            m.appendReplacement(sb, "$2:$1;");
        }
        m.appendTail(sb);
        p("appendReplacement", sb.toString());

        p("quoteReplacement", "a.b".replaceAll("\\.", Matcher.quoteReplacement("$1\\")));

        p("replace.named", "2024-06-15".replaceAll(
                "(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})", "${d}.${m}.${y}"));
        p("replace.case", "one two three".replaceAll("(\\w+) (\\w+) (\\w+)", "$3-$2-$1"));
        p("replace.empty", "abc".replaceAll("", "-"));
        p("replace.lookahead", "a1b2".replaceAll("(?=\\d)", "_"));

        // Region-bound matching.
        Matcher region = Pattern.compile("\\d+").matcher("abc123def456");
        region.region(3, 6);
        p("region.first", region.find());
        p("region.value", region.group());
        p("region.start", region.regionStart());
        p("region.end", region.regionEnd());

        // Bounded repetition and alternation in classes.
        p("bounded", "aaa".matches("a{2,3}"));
        p("bounded2", "a".matches("a{2,3}"));
        p("boundedLazy", "aaaa".replaceAll("a{2,3}?", "X"));
        p("class.union", "a-d-m-p".replaceAll("[a-d[m-p]]", "?"));
        p("class.subtract", "azbc".replaceAll("[a-z&&[^bc]]", "."));
    }
}

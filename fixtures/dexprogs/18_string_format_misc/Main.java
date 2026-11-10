public class Main {
    public static void main(String[] args) {
        System.out.println("parseInt=" + Integer.parseInt("12345"));
        System.out.println("parseIntNeg=" + Integer.parseInt("-12345"));
        System.out.println("parseIntRadix=" + Integer.parseInt("ff", 16));
        System.out.println("toStringRadix=" + Integer.toString(255, 16));
        System.out.println("toHexString=" + Integer.toHexString(255));
        System.out.println("toHexStringNeg=" + Integer.toHexString(-1));
        System.out.println("toOctalString=" + Integer.toOctalString(64));
        System.out.println("toBinaryString=" + Integer.toBinaryString(10));
        System.out.println("toString=" + Integer.toString(-42));
        System.out.println("longToString=" + Long.toString(9223372036854775807L));
        System.out.println("intToLong=" + Long.valueOf(7));
        System.out.println("abs=" + Math.abs(-7) + " " + Math.abs(Integer.MIN_VALUE));
        System.out.println("max=" + Math.max(3, 9) + " min=" + Math.min(3, 9));
        System.out.println("maxDouble=" + Math.max(1.5, 2.5));
        System.out.println("sqrt144=" + Math.sqrt(144.0));
        System.out.println("sqrt2=" + Math.sqrt(2.0));
        System.out.println("absDouble=" + Math.abs(-2.5));
        System.out.println("floor=" + Math.floor(2.9) + " ceil=" + Math.ceil(2.1));
        System.out.println("round=" + Math.round(2.5) + " " + Math.round(-2.5));
        System.out.println("pow=" + Math.pow(2, 8));
        System.out.println("isDigit=" + Character.isDigit('7') + " " + Character.isDigit('x'));
        System.out.println("isLetter=" + Character.isLetter('a') + " " + Character.isLetter('1'));
        System.out.println("isWhitespace=" + Character.isWhitespace(' '));
        System.out.println("toUpperCase=" + Character.toUpperCase('q'));
        System.out.println("toLowerCase=" + Character.toLowerCase('Q'));
        System.out.println("getNumericValue=" + Character.getNumericValue('7'));
        System.out.println("digit=" + Character.digit('f', 16));
        System.out.println("forDigit=" + Character.forDigit(15, 16));
        System.out.println("compare=" + Character.compare('a', 'b'));
        System.out.println("isUpperCase=" + Character.isUpperCase('A'));
        System.out.println("charHashCode=" + Character.hashCode('A'));
        System.out.println("minNegZero=" + Math.min(-0.0, 0.0));
        System.out.println("hypot=" + Math.hypot(3.0, 4.0));
        System.out.println("cbrt=" + Math.cbrt(27.0));
        System.out.println("exp0=" + Math.exp(0.0));
        System.out.println("log1=" + Math.log(1.0));
        System.out.println("signum=" + Math.signum(-3.5));
    }
}

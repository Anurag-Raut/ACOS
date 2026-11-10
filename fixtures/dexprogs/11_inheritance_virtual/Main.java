public class Main {
    static abstract class Shape {
        final String name;

        Shape(String name) {
            this.name = name;
        }

        abstract double area();

        double perimeter() {
            return 0.0;
        }

        @Override
        public String toString() {
            return name + ":" + area();
        }
    }

    static class Rect extends Shape {
        final double w, h;

        Rect(double w, double h) {
            super("Rect");
            this.w = w;
            this.h = h;
        }

        @Override
        double area() {
            return w * h;
        }

        @Override
        double perimeter() {
            return 2 * (w + h);
        }

        @Override
        public String toString() {
            return "[" + super.toString() + "]";
        }
    }

    static class Square extends Rect {
        Square(double s) {
            super(s, s);
        }

        @Override
        public String toString() {
            return "Square(" + area() + ")";
        }
    }

    static class Circle extends Shape {
        final double r;

        Circle(double r) {
            super("Circle");
            this.r = r;
        }

        @Override
        double area() {
            return 3.0 * r * r;
        }

        @Override
        double perimeter() {
            return 6.0 * r;
        }
    }

    static String describe(Shape s) {
        return s.getClass().getSimpleName() + " area=" + s.area();
    }

    public static void main(String[] args) {
        Shape[] shapes = {new Rect(2, 3), new Square(4), new Circle(1)};
        for (Shape s : shapes) {
            System.out.println("shape=" + s);
        }
        for (Shape s : shapes) {
            System.out.println("describe=" + describe(s));
        }
        for (Shape s : shapes) {
            System.out.println("perimeter=" + s.perimeter());
        }
        Rect r = new Rect(2, 3);
        System.out.println("rect.superToString=" + ((Shape) r).toString());
        Shape up = new Square(5);
        System.out.println("up.area=" + up.area());
        System.out.println("up instanceof Shape=" + (up instanceof Shape));
        System.out.println("up instanceof Rect=" + (up instanceof Rect));
        System.out.println("up instanceof Square=" + (up instanceof Square));
        System.out.println("up instanceof Circle=" + (up instanceof Circle));
        System.out.println("up.class=" + up.getClass().getName());
        System.out.println("r.class=" + r.getClass().getName());
    }
}

public class Main {
    interface Animal {
        String name();

        default String speak() {
            return name() + " makes a sound";
        }

        static String kind() {
            return "animal";
        }
    }

    interface Pet {
        default String greet() {
            return "hi from pet";
        }

        String name();
    }

    static class Dog implements Animal, Pet {
        @Override
        public String name() {
            return "Dog";
        }

        @Override
        public String speak() {
            return name() + " barks";
        }

        @Override
        public String greet() {
            return "hi from " + name();
        }
    }

    static class Cat implements Animal {
        @Override
        public String name() {
            return "Cat";
        }
    }

    static class Robot implements Animal, Pet {
        @Override
        public String name() {
            return "Robot";
        }

        @Override
        public String speak() {
            return "beep";
        }
    }

    static String call(Animal a) {
        return a.name() + ": " + a.speak();
    }

    public static void main(String[] args) {
        Animal[] animals = {new Dog(), new Cat(), new Robot()};
        for (Animal a : animals) {
            System.out.println(call(a));
        }
        System.out.println("kind=" + Animal.kind());
        Pet p = new Dog();
        System.out.println("pet=" + p.greet());
        p = new Robot();
        System.out.println("pet=" + p.greet());
        Cat c = new Cat();
        System.out.println("cat.default=" + c.speak());
        Dog d = new Dog();
        System.out.println("dog.default=" + d.greet());
        System.out.println("dog instanceof Animal=" + (d instanceof Animal));
        System.out.println("dog instanceof Pet=" + (d instanceof Pet));
        System.out.println("dog instanceof Dog=" + (d instanceof Dog));
        System.out.println("dog instanceof Cat=" + (((Object) d) instanceof Cat));
        System.out.println("iface.class.isInterface=" + Animal.class.isInterface());
        System.out.println("dog.name=" + d.name());
        System.out.println("cat.name=" + c.name());
        System.out.println("robot.name=" + new Robot().name());
    }
}

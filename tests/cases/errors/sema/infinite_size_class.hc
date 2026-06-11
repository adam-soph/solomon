//@ error: has an infinite size
// A class that contains itself by value has no finite layout (use a pointer field).
class Node { Node next; };

U0 Main() {}

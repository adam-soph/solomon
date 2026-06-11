//@ error: function-pointer `typedef` puts the name after
// The C shape with the name buried inside the declarator is rejected; HolyC wants
// `typedef I64 (*)(I64) Name;` or the keyword-less `I64 (*Name)(I64);`.
typedef I64 (*Name)(I64);

U0 Main() {}

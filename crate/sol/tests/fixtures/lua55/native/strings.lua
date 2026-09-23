local a = "A\0\255\u{1f600}"
local b = [==[
hello ]=] world]==]
local c = string.sub("abcdef", 2, -2)

function concat(x, y)
    return x .. y
end

function compare(x, y)
    return x == y
end

return #a == 7 and #b == 15 and c == "bcde" and string.lower("ABC") == "abc" and string.upper("abc") == "ABC" and
           string.reverse("abc") == "cba" and string.rep("ab", 3, ":") == "ab:ab:ab" and string.rep("x", 0) == "" and
           string.len("hi") == 2 and "abc" < "abd" and "a\0b" ~= "a\0c" and not ("abc" == "ab") and
           concat("answer=", 42) == "answer=42" and compare("ab" .. "c", "abc") and ("" or false) == "" and
           (false or "yes") == "yes"

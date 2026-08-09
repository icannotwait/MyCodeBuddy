#pragma once

#include <exception>
#include <iostream>
#include <string>
#include <utility>
#include <vector>

#define CODEG_EUI_TEST_HARNESS_VERSION 1

namespace codeg_eui::test {

struct Case {
    const char* name;
    void (*body)();
};

struct AbortCase final {};

inline std::vector<Case>& registry() {
    static std::vector<Case> value;
    return value;
}

inline int& failures() {
    static int value = 0;
    return value;
}

struct Registrar {
    Registrar(const char* name, void (*body)()) {
        registry().push_back({name, body});
    }
};

inline void expect(bool ok, const char* expression, const char* file, int line) {
    if (!ok) {
        ++failures();
        std::cerr << file << ':' << line << ": " << expression << '\n';
    }
}

template <class A, class B>
inline void expectEq(const A& actual,
                     const B& expected,
                     const char* expression,
                     const char* file,
                     int line,
                     bool fatal) {
    if (!(actual == expected)) {
        expect(false, expression, file, line);
        if (fatal) {
            throw AbortCase{};
        }
    }
}

inline int runAll() {
    int failedCases = 0;
    for (const Case& test : registry()) {
        const int before = failures();
        try {
            test.body();
        } catch (const AbortCase&) {
        } catch (const std::exception& error) {
            expect(false, error.what(), __FILE__, __LINE__);
        }
        if (failures() != before) {
            ++failedCases;
            std::cerr << "[FAIL] " << test.name << '\n';
        } else {
            std::cout << "[PASS] " << test.name << '\n';
        }
    }
    return failedCases == 0 ? 0 : 1;
}

}  // namespace codeg_eui::test

#define TEST(suite, name)                                                        \
    static void suite##_##name();                                                \
    static ::codeg_eui::test::Registrar suite##_##name##_registrar(              \
        #suite "." #name, &suite##_##name);                                     \
    static void suite##_##name()
#define EXPECT_TRUE(value)                                                       \
    ::codeg_eui::test::expect(!!(value), #value, __FILE__, __LINE__)
#define EXPECT_FALSE(value)                                                      \
    ::codeg_eui::test::expect(!(value), "!(" #value ")", __FILE__, __LINE__)
#define EXPECT_EQ(actual, expected)                                              \
    ::codeg_eui::test::expectEq(                                                 \
        (actual), (expected), #actual " == " #expected, __FILE__, __LINE__, false)
#define EXPECT_GE(actual, expected)                                              \
    ::codeg_eui::test::expect(                                                   \
        ((actual) >= (expected)), #actual " >= " #expected, __FILE__, __LINE__)
#define ASSERT_EQ(actual, expected)                                              \
    ::codeg_eui::test::expectEq(                                                 \
        (actual), (expected), #actual " == " #expected, __FILE__, __LINE__, true)

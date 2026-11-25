"""Where the private literals are read from when the variable is missing."""

load("@bazel_skylib//lib:unittest.bzl", "asserts", "unittest")
load("//tools:private_literals_source.bzl", "literals_source")

def _declared_wins(ctx):
    env = unittest.begin(ctx)
    source = literals_source("/lists/mine.txt", "/alondra", "/home/ana")
    asserts.equals(env, "/lists/mine.txt", source.path)
    asserts.true(env, source.required)
    return unittest.end(env)

def _none_is_not_a_file(ctx):
    env = unittest.begin(ctx)
    source = literals_source("none", "/alondra", "/home/ana")
    asserts.equals(env, "none", source.path)
    asserts.false(env, source.required)
    return unittest.end(env)

def _node_without_the_variable_reads_alondra_home(ctx):
    env = unittest.begin(ctx)
    source = literals_source("", "/alondra/", "/home/ana")
    asserts.equals(env, "/alondra/files/private-literals.txt", source.path)
    asserts.false(env, source.required)
    return unittest.end(env)

def _home_is_the_last_place_to_look(ctx):
    env = unittest.begin(ctx)
    source = literals_source("", "", "/home/ana")
    asserts.equals(env, "/home/ana/.alondra/files/private-literals.txt", source.path)
    asserts.false(env, source.required)
    return unittest.end(env)

def _nowhere_to_look(ctx):
    env = unittest.begin(ctx)
    asserts.equals(env, "", literals_source("", "", "").path)
    return unittest.end(env)

declared_wins_test = unittest.make(_declared_wins)
none_is_not_a_file_test = unittest.make(_none_is_not_a_file)
node_reads_alondra_home_test = unittest.make(_node_without_the_variable_reads_alondra_home)
home_is_last_test = unittest.make(_home_is_the_last_place_to_look)
nowhere_test = unittest.make(_nowhere_to_look)

def private_literals_source_suite(name):
    unittest.suite(
        name,
        declared_wins_test,
        none_is_not_a_file_test,
        node_reads_alondra_home_test,
        home_is_last_test,
        nowhere_test,
    )

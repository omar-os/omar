"""Regressions for respawn-sensitive benchmark accounting."""
import unittest
from summarize import retirement_metrics


def event(seconds, alive, output=False):
    return {'seconds': seconds, 'state': {
        'sessions': ['bench-0-left'] if alive else [],
        'outputs': {'left': {}} if output else {},
    }}


class RetirementMetrics(unittest.TestCase):
    def test_dead_incarnation_is_not_retirement(self):
        trace = [event(1, True), event(2, False), event(3, True), event(5, True, True)]
        self.assertEqual(retirement_metrics(trace, 8), {
            'result_to_retirement_seconds': {}, 'unretired_result_seconds': {'left': 3}})

    def test_retirement_after_respawn_result(self):
        trace = [event(1, True), event(2, False), event(3, True),
                 event(5, True, True), event(7, False, True)]
        self.assertEqual(retirement_metrics(trace, 8), {
            'result_to_retirement_seconds': {'left': 2}, 'unretired_result_seconds': {}})

    def test_respawn_clears_previous_retirement(self):
        trace = [event(1, True, True), event(2, False, True), event(3, True, True)]
        self.assertEqual(retirement_metrics(trace, 8), {
            'result_to_retirement_seconds': {}, 'unretired_result_seconds': {'left': 7}})


if __name__ == '__main__':
    unittest.main()

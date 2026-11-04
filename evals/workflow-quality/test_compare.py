import unittest
from compare import CASES, check, compare

class SmokeComparisonTests(unittest.TestCase):
    def valid(self):
        return {"decision":"proceed","recommendation":"Proceed for this case", "claims":[{"text":"Score is 92","source_ids":["measurement"]}],"limitations":["Only the supplied case"]}
    def test_wrong_decisions_unknown_citations_and_malformed_claims_fail(self):
        self.assertTrue(check(CASES[0],self.valid()))
        for edit in [{"decision":"hold"},{"claims":[{"text":"Claim","source_ids":["invented"]}]},{"claims":[None]},{"recommendation":42}]:
            self.assertFalse(check(CASES[0],self.valid()|edit))
    def test_missing_and_unreviewed_cases_cannot_be_accepted(self):
        r=compare([{"configuration":"workflow","case_id":"approved","output":self.valid()}])
        self.assertEqual(len(r['rows']),6)
        self.assertEqual(sum(x['missing'] for x in r['rows']),5)
        self.assertEqual(r['summary']['workflow']['reviewed_acceptances'],0)
    def test_duplicates_and_fabricated_review_types_are_rejected(self):
        record={"configuration":"workflow","case_id":"approved","output":self.valid()}
        with self.assertRaises(ValueError): compare([record,record])
        with self.assertRaises(ValueError): compare([record|{"human_accepted":"yes"}])

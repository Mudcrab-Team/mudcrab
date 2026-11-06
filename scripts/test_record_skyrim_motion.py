import unittest

from record_skyrim_motion import parse_ocr, verify_trace


def recording():
    rows = [dict(kind=1, text='mudcrab-motion-256-v1', sample=1000, qpc=0, code=1,
                 state=0, thread=10, values=[0.0] * 40)]
    for frame in range(40):
        if frame in (12, 22):
            values = [0.0] * 40
            values[:4] = [1.0, 0.0, 0.0, 1.0] if frame == 12 else [0.0, 0.5, 0.0, 0.0]
            rows.append(dict(kind=2, text='Forward', sample=frame, qpc=frame * 50,
                             code=17, state=0, thread=10, values=values))
        values = [0.0] * 40
        values[1] = min(max(frame - 12, 0), 10) * 4.0
        values[10] = values[1]
        values[11] = 120.0
        values[13] = values[1]
        values[14] = 120.0
        rows.append(dict(kind=3, text='', sample=frame, qpc=frame * 50 + 1,
                         code=3, state=0, thread=10, values=values))
    rows.append(dict(kind=5, text='complete after native W release', sample=39, qpc=2000,
                     code=0, state=0, thread=10, values=[0.0] * 40))
    inputs, samples = 0, 0
    for i, row in enumerate(rows, 1):
        row['sequence'] = i
        inputs += row['kind'] == 2
        samples += row['kind'] == 3
        row['input_sequence'] = inputs
        row['sample'] = samples if row['kind'] != 1 else 1000
    return rows


class MotionProofTests(unittest.TestCase):
    def test_dialog_quotes_do_not_swallow_the_no_button(self):
        text, words = parse_ocr('left\ttop\twidth\theight\ttext\n'
                               '10\t20\t30\t40\t"Survival\n'
                               '11\t20\t30\t40\tMode"\n'
                               '671\t539\t17\t17\tNo\n')
        self.assertEqual(text, '"survival mode" no')
        self.assertEqual(words[-1]['text'], 'No')
        self.assertEqual(words[-1]['top'], '539')

    def test_native_received_press_moves_player_and_camera(self):
        result = verify_trace(recording())
        self.assertEqual(result['status'], 'passed')
        self.assertEqual(result['horizontal_displacement'], 40.0)

    def test_sent_but_unreceived_input_cannot_pass(self):
        rows = [r for r in recording() if r['kind'] != 2]
        for i, row in enumerate(rows, 1):
            row['sequence'] = i
            row['input_sequence'] = 0
        with self.assertRaisesRegex(ValueError, 'did not deliver W'):
            verify_trace(rows)

    def test_received_input_without_gameplay_acceptance_fails(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 2:
                row['values'][3] = 0.0
        with self.assertRaisesRegex(ValueError, 'did not accept forward'):
            verify_trace(rows)

    def test_blocked_player_does_not_count_as_movement(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 3:
                row['values'][1] = 0.0
        with self.assertRaisesRegex(ValueError, 'No horizontal player movement'):
            verify_trace(rows)

    def test_camera_must_follow(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 3:
                row['values'][10] = 0.0
        with self.assertRaisesRegex(ValueError, 'Camera root did not follow'):
            verify_trace(rows)

    def test_third_person_is_rejected(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 3:
                row['state'] = 9
        with self.assertRaisesRegex(ValueError, 'Insufficient first-person'):
            verify_trace(rows)

    def test_input_worker_thread_is_preserved(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 2:
                row['thread'] = 11
        result = verify_trace(rows)
        self.assertEqual(result['input_threads'], [11])
        self.assertEqual(result['camera_threads'], [10])

    def test_input_sample_association_is_required(self):
        rows = recording()
        rows[20]['input_sequence'] = 0
        with self.assertRaisesRegex(ValueError, 'Broken input/sample association'):
            verify_trace(rows)

    def test_attached_camera_is_required(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 3:
                row['code'] = 1
        with self.assertRaisesRegex(ValueError, 'Attached NiCamera is missing'):
            verify_trace(rows)

    def test_missing_release_fails(self):
        rows = recording()
        for row in rows:
            if row['kind'] == 2:
                row['values'][0] = 1.0
        with self.assertRaisesRegex(ValueError, 'down/up pair'):
            verify_trace(rows)

    def test_dropped_records_fail(self):
        rows = recording()
        del rows[10]
        with self.assertRaisesRegex(ValueError, 'Lost native records'):
            verify_trace(rows)

    def test_interrupted_native_capture_fails(self):
        with self.assertRaisesRegex(ValueError, 'has not completed'):
            verify_trace(recording()[:-1])


if __name__ == '__main__':
    unittest.main()

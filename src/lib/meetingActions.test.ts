// @vitest-environment jsdom
import { it, expect, vi } from 'vitest';
import { followMeetingEvidence } from './meetingActions';
it('saved evidence links scroll inside their own editor without changing stored HTML',()=>{
  const root=document.createElement('div');root.contentEditable='true';
  root.innerHTML='<a href="#meeting-demo-1"><strong>Evidence</strong></a><p id="meeting-demo-1">Full passage</p>';
  const before=root.innerHTML;
  const scroll=vi.fn();root.querySelector('p')!.scrollIntoView=scroll;
  const event={target:root.querySelector('strong'),preventDefault:vi.fn()} as unknown as MouseEvent;
  expect(followMeetingEvidence(event,root)).toBe(true);
  expect(scroll).toHaveBeenCalledOnce();expect(event.preventDefault).toHaveBeenCalledOnce();
  expect(root.innerHTML).toBe(before);
});
